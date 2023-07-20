use clap::Parser;
use std::{io, net::SocketAddr};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
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

async fn handle_client(sap_router: SocketAddr, mut stream: TcpStream) -> io::Result<()> {
    let mut buffer = Vec::new();

    let n = stream.read_buf(&mut buffer).await?;

    let (_, hello) =
        socks_parser::decode_socks_request_hello(&buffer[..n]).map_err(map_nom_error)?;
    if hello
        .methods
        .contains(&socks_parser::AuthenticationMethod::None)
    {
        let response = socks_parser::response::Hello {
            version: socks_parser::Version::Socks5,
            method: socks_parser::AuthenticationMethod::None,
        };
        buffer.clear();
        socks_parser::encode_socks_response_hello(&response, &mut buffer);
        stream.write_all(&buffer[..]).await?;
    } else {
        let response = socks_parser::response::Hello {
            version: socks_parser::Version::Socks5,
            method: socks_parser::AuthenticationMethod::None,
        };
        socks_parser::encode_socks_response_hello(&response, &mut buffer);
        stream.write_all(&buffer[..]).await?;
        return Ok(());
    }

    buffer.clear();
    let n = stream.read_buf(&mut buffer).await?;

    let (_, request) = socks_parser::decode_socks_request(&buffer[..n]).map_err(map_nom_error)?;
    if request.command != socks_parser::Command::Connect {
        let response = socks_parser::response::Response {
            version: socks_parser::Version::Socks5,
            status: socks_parser::response::Status::CommandNotSupported,
            addr: request.addr,
            port: request.port,
        };
        buffer.clear();
        socks_parser::encode_socks_response(&response, &mut buffer);
        stream.write_all(&buffer[..]).await?;
        return Ok(());
    }
    let host = match request.addr {
        socks_parser::AddressType::IPv4(ip4) => format!("{}", ip4),
        socks_parser::AddressType::DomainName(ref n) => n.clone(),
        socks_parser::AddressType::IPv6(ip6) => format!("[{}]", ip6),
    };

    let sap_stream = match sap_connect(sap_router, &host, request.port).await {
        Ok(s) => {
            let response = socks_parser::response::Response {
                version: socks_parser::Version::Socks5,
                status: socks_parser::response::Status::Success,
                addr: request.addr,
                port: request.port,
            };
            buffer.clear();
            socks_parser::encode_socks_response(&response, &mut buffer);
            stream.write_all(&buffer[..]).await?;
            s
        }
        Err(e) => {
            let response = socks_parser::response::Response {
                version: socks_parser::Version::Socks5,
                status: socks_parser::response::Status::GeneralFailure,
                addr: request.addr,
                port: request.port,
            };
            buffer.clear();
            socks_parser::encode_socks_response(&response, &mut buffer);
            stream.write_all(&buffer[..]).await?;
            return Err(e);
        }
    };

    let mut sap_ni_stream = sap::SapNiStream::new(sap_stream);

    tokio::io::copy_bidirectional(&mut stream, &mut sap_ni_stream).await?;

    Ok(())
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let args = Options::parse();
    dbg!(&args);

    let listener = TcpListener::bind(("127.0.0.1", args.bind_port)).await?;

    loop {
        let (client, addr) = listener.accept().await?;
        let sap_router = args.sap_router.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(sap_router, client).await {
                eprintln!("Issue with client {addr}: {e}");
            }
        });
    }
}
