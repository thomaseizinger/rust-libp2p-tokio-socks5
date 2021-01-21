//! Ping-Pong - libp2p TCP/IP over Tor
//!
//! Ping application (dialer/listener) using libp2p and running over the Tor
//! network.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

use std::{
    collections::HashMap,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use anyhow::Result;
use futures::{future, prelude::*};
use libp2p::{
    core::{
        muxing::StreamMuxerBox,
        transport::Boxed,
        upgrade::{SelectUpgrade, Version},
    },
    dns::DnsConfig,
    identity::Keypair,
    mplex::MplexConfig,
    noise::{self, NoiseConfig, X25519Spec},
    ping::{Ping, PingConfig},
    swarm::SwarmBuilder,
    yamux::YamuxConfig,
    Multiaddr, PeerId, Swarm, Transport,
};
use log::warn;
use structopt::StructOpt;

use libp2p_tokio_socks5::Socks5TokioTcpConfig;

/// The ping-pong onion service address.
const ONION: &str = "/onion3/7gr3dngwhk74thi4vv6bm3v3bicaxe4apvcemoxo3hadpvsyfifjqnid:7";
const LOCAL_PORT: u16 = 7777;

/// Tor should be started with a hidden service configured. Add the following to
/// your torrc
///
///     HiddenServiceDir /var/lib/tor/hidden_service/
///     HiddenServicePort 7 127.0.0.1:7777
///
/// See https://2019.www.torproject.org/docs/tor-onion-service for details on configuring
/// tor onion services (previously tor hidden services). Next set the const
/// ONION above to the address generated by tor after initial startup. The onion
/// address can be found in the hostname file in the hidden service data
/// directory e.g., `/var/lib/tor/hidden_service/hostname` (if this file does
/// not exist you probably have something wrong with your tor  configuration,
/// check the permissions on the hidden service data dir :)
#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let opt = Opt::from_args();

    let addr = opt.onion.unwrap_or_else(|| ONION.to_string());
    let addr = addr
        .parse::<Multiaddr>()
        .expect("failed to parse multiaddr");

    if opt.dialer {
        run_dialer(addr).await?;
    } else {
        run_listener(addr).await?;
    }

    Ok(())
}

#[derive(Debug, StructOpt)]
#[structopt(name = "ping-pong", about = "libp2p ping-pong application over Tor.")]
pub struct Opt {
    /// Run as the dialer i.e., do the ping
    #[structopt(short, long)]
    pub dialer: bool,

    /// Run as the listener i.e., do the pong (default)
    #[structopt(short, long)]
    pub listener: bool,

    /// Onion mulitaddr to use (only required for dialer)
    #[structopt(long)]
    pub onion: Option<String>,
}

/// Entry point to run the ping-pong application as a dialer.
async fn run_dialer(addr: Multiaddr) -> Result<()> {
    let map = HashMap::new();
    let config = PingConfig::new()
        .with_keep_alive(true)
        .with_interval(Duration::from_secs(1));
    let mut swarm = build_swarm(config, map)?;

    Swarm::dial_addr(&mut swarm, addr).unwrap();

    future::poll_fn(move |cx: &mut Context<'_>| loop {
        match swarm.poll_next_unpin(cx) {
            Poll::Ready(Some(event)) => println!("{:?}", event),
            Poll::Ready(None) => return Poll::Ready(()),
            Poll::Pending => return Poll::Pending,
        }
    })
    .await;

    Ok(())
}

/// Entry point to run the ping-pong application as a listener.
async fn run_listener(onion: Multiaddr) -> Result<()> {
    let map = onion_port_map(onion.clone());
    log::info!("Onion service: {}", onion);

    let config = PingConfig::new().with_keep_alive(true);
    let mut swarm = build_swarm(config, map)?;

    Swarm::listen_on(&mut swarm, onion.clone())?;

    future::poll_fn(move |cx: &mut Context<'_>| loop {
        match swarm.poll_next_unpin(cx) {
            Poll::Ready(Some(event)) => println!("{:?}", event),
            Poll::Ready(None) => return Poll::Ready(()),
            Poll::Pending => return Poll::Pending,
        }
    })
    .await;

    Ok(())
}

/// Build a libp2p swarm.
pub fn build_swarm(config: PingConfig, map: HashMap<Multiaddr, u16>) -> Result<Swarm<Ping>> {
    let id_keys = Keypair::generate_ed25519();
    let peer_id = PeerId::from(id_keys.public());

    let transport = build_transport(id_keys, map)?;
    let behaviour = Ping::new(config);

    let swarm = SwarmBuilder::new(transport, behaviour, peer_id)
        .executor(Box::new(TokioExecutor))
        .build();

    Ok(swarm)
}

fn onion_port_map(onion: Multiaddr) -> HashMap<Multiaddr, u16> {
    let mut map = HashMap::new();
    map.insert(onion, LOCAL_PORT);
    map
}

struct TokioExecutor;

impl libp2p::core::Executor for TokioExecutor {
    fn exec(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) {
        tokio::spawn(future);
    }
}

/// Builds a libp2p transport with the following features:
/// - TCP connectivity over the Tor network
/// - DNS name resolution
/// - Authentication via noise
/// - Multiplexing via yamux or mplex
fn build_transport(
    id_keys: Keypair,
    map: HashMap<Multiaddr, u16>,
) -> anyhow::Result<PingPongTransport> {
    let dh_keys = noise::Keypair::<X25519Spec>::new().into_authentic(&id_keys)?;
    let noise = NoiseConfig::xx(dh_keys).into_authenticated();

    let tcp = Socks5TokioTcpConfig::default().nodelay(true).onion_map(map);
    let transport = DnsConfig::new(tcp)?;

    let transport = transport
        .upgrade(Version::V1)
        .authenticate(noise)
        .multiplex(SelectUpgrade::new(
            YamuxConfig::default(),
            MplexConfig::new(),
        ))
        .map(|(peer, muxer), _| (peer, StreamMuxerBox::new(muxer)))
        .boxed();

    Ok(transport)
}

/// libp2p `Transport` for the ping-pong application.
pub type PingPongTransport = Boxed<(PeerId, StreamMuxerBox)>;
