use clap::Parser;
use socks_parser::{ConnectionRequest, Destination, Server};
use std::{
    io,
    net::SocketAddr,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
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
    sap_router: SocketAddr,
    #[clap(short, long, default_value_t = 1080)]
    bind_port: u16,
}

static SAP_ROUTER: AtomicPtr<SocketAddr> = AtomicPtr::new(ptr::null_mut());

fn map_nom_error(e: nom::Err<nom::error::VerboseError<&[u8]>>) -> io::Error {
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

    let data = sap_buffer.extract_from_reader(&mut stream).await?;

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
            Err(io::Error::new(io::ErrorKind::Other, msg))
        }
        r => Err(io::Error::new(
            io::ErrorKind::Other,
            format!("Unexpected SapRouter variant: {r:?}"),
        )),
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

#[tokio::main]
async fn main() -> io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args = Options::parse();
    dbg!(&args);

    let listener = TcpListener::bind(("127.0.0.1", args.bind_port)).await?;
    let server = Server::new(listener);

    SAP_ROUTER.store(Box::into_raw(Box::new(args.sap_router)), Ordering::Relaxed);

    server.run(handle_request, handle_stream).await?;

    Ok(())
}
