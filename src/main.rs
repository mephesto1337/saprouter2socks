use clap::Parser;
use socks_parser::{ConnectionRequest, Destination, Server};
use std::{
    io,
    net::SocketAddr,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
};

mod sap;
use sap::Wire;

use crate::sap::SapRouter;

#[derive(Debug, Parser)]
struct Options {
    /// SAP router port to use
    #[clap(short, long, default_value_t = 3299)]
    sap_port: u16,

    /// Local port to use for socks proxy
    #[clap(short, long, default_value = "127.0.0.1:1080")]
    bind_addr: SocketAddr,

    /// Timeout for connection
    #[clap(short, long, default_value_t = 1000)]
    timeout_ms: u64,

    /// SAP Router address
    sap_router: String,
}

static SAP_ROUTER: AtomicPtr<SocketAddr> = AtomicPtr::new(ptr::null_mut());

fn map_nom_error(e: nom::Err<nom::error::Error<&[u8]>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{e:x?}"))
}

async fn sap_connect(addr: SocketAddr, host: &str, port: u16) -> io::Result<TcpStream> {
    const NI_VERSION: u8 = 40;

    let mut stream = TcpStream::connect(&addr).await?;
    let hops = vec![
        addr.into(),
        sap::SapRouterHop {
            hostname: host.into(),
            port: format!("{}", port).into(),
            password: "".into(),
        },
    ];

    let mut sap_buffer = sap::SapNi::default();
    sap_buffer.set(&sap::SapRouter::Route {
        ni_version: NI_VERSION,
        talk_mode: sap::TalkMode::MsgIo,
        rest_nodes: 1,
        current_position: 1,
        hops,
    });

    stream.write_all(sap_buffer.as_ref()).await?;

    let data = sap_buffer.read_from_ni_reader(&mut stream).await?;

    match SapRouter::decode(data).map_err(map_nom_error)?.1 {
        SapRouter::Pong => Ok(stream),
        SapRouter::ErrorInformation {
            return_code, text, ..
        } => {
            let msg = if text.is_empty() {
                format!("return code: {return_code}")
            } else {
                text.into()
            };
            Err(io::Error::other(msg))
        }
        r => Err(io::Error::other(format!(
            "Unexpected SapRouter variant: {r:?}"
        ))),
    }
}

async fn handle_request(req: ConnectionRequest) -> io::Result<(TcpStream, Destination)> {
    let addr = *unsafe { &*SAP_ROUTER.load(Ordering::Relaxed) };
    let host = match req.destination.addr {
        socks_parser::v5::AddressType::IPv4(i) => format!("{i}"),
        socks_parser::v5::AddressType::DomainName(n) => n,
        socks_parser::v5::AddressType::IPv6(i) => format!("[{i}]"),
    };
    let stream = sap_connect(addr, host.as_str(), req.destination.port).await?;

    let remote_addr = stream.peer_addr()?;
    log::info!(
        "{host}:{port} -> {res}:{port}",
        res = remote_addr.ip(),
        port = remote_addr.port(),
    );
    Ok((stream, remote_addr.into()))
}

async fn handle_stream(mut local: TcpStream, remote: TcpStream) -> io::Result<()> {
    let mut sap_ni = sap::SapNiStream::new(remote);
    sap_ni.pipe(&mut local).await
}

async fn connect_timeout(addr: SocketAddr, timeout_ms: u64) -> io::Result<TcpStream> {
    let sleep = tokio::time::sleep(Duration::from_millis(timeout_ms));
    tokio::pin!(sleep);

    tokio::select! {
        v = TcpStream::connect(addr) => {
            v
        }
        _ = &mut sleep => {
            Err(io::Error::new(io::ErrorKind::TimedOut, format!("Timeout exceeded while connecting to {addr}")))
        }
    }
}

async fn find_sap_router(host: &str, port: u16, timeout_ms: u64) -> io::Result<()> {
    let addrs: Vec<_> = tokio::net::lookup_host((host, port))
        .await?
        .map(|addr| {
            Box::pin(async move {
                log::debug!("Trying SAP Router at {addr}...");
                connect_timeout(addr, timeout_ms).await
            })
        })
        .collect();
    let stream = match futures::future::select_ok(addrs.into_iter()).await {
        Ok((s, _)) => s,
        Err(e) => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Could not found a valid SAP Router at {host}:{port}: {e}",),
            ));
        }
    };

    let addr = stream.peer_addr()?;
    log::info!("Found valid SAP Router at {addr}");
    SAP_ROUTER.store(Box::into_raw(Box::new(addr)), Ordering::Relaxed);

    Ok(())
}

#[tokio::main]
async fn main() -> io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args = Options::parse();

    find_sap_router(args.sap_router.as_str(), args.sap_port, args.timeout_ms).await?;

    let listener = TcpListener::bind(&args.bind_addr).await?;
    if let Ok(addr) = listener.local_addr() {
        log::info!("Listening on {addr}");
    } else {
        log::info!("Listening on {addr}", addr = &args.bind_addr);
    }
    let server = Server::new(listener);

    server.run(handle_request, handle_stream).await?;

    Ok(())
}
