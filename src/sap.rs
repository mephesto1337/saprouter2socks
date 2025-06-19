use std::{
    borrow::Cow,
    io,
    mem::size_of,
    net::{SocketAddr, SocketAddrV4, SocketAddrV6},
};

use nom::{
    bytes::streaming::{tag, take, take_while},
    combinator::{map, map_opt, verify},
    error::{ContextError, ErrorKind, VerboseError, context},
    multi::{length_data, many_m_n},
    number::streaming::{be_u8, be_u16, be_u32},
    sequence::{preceded, terminated, tuple},
};
use tokio::io::{AsyncRead, AsyncReadExt};

pub trait Wire<'a>: Sized {
    fn encode_into(&self, buffer: &mut Vec<u8>);
    fn decode(input: &'a [u8]) -> nom::IResult<&'a [u8], Self, VerboseError<&'a [u8]>>;
}

mod stream;
pub use stream::SapNiStream;

#[derive(Debug, PartialEq, Eq)]
pub struct SapNi {
    buffer: Vec<u8>,
}

impl Default for SapNi {
    fn default() -> Self {
        let buffer = 0u32.to_be_bytes().into();
        Self { buffer }
    }
}

impl AsRef<[u8]> for SapNi {
    fn as_ref(&self) -> &[u8] {
        &self.buffer[..]
    }
}

impl SapNi {
    pub fn set<'w, W: Wire<'w>>(&mut self, obj: &'w W) {
        self.buffer.clear();
        self.buffer.extend_from_slice(&0u32.to_be_bytes()[..]);
        let offset = self.buffer.len();
        obj.encode_into(&mut self.buffer);
        let obj_length = (self.buffer.len() - offset) as u32;
        self.buffer[..size_of::<u32>()].copy_from_slice(&obj_length.to_be_bytes()[..]);
    }

    fn get_buffer_len(&self) -> Option<usize> {
        let (_, obj_size) = be_u32::<_, ()>(&self.buffer[..]).ok()?;
        Some(obj_size as usize)
    }

    pub fn get_data(&self) -> Option<&[u8]> {
        let obj_size = self.get_buffer_len()?;
        if obj_size + size_of::<u32>() < self.buffer.len() {
            None
        } else {
            Some(&self.buffer[size_of::<u32>()..])
        }
    }

    fn set_data(&mut self, buf: &[u8]) {
        self.buffer.clear();
        self.buffer.reserve(buf.len() + size_of::<u32>());
        let size = buf.len() as u32;
        self.buffer.extend_from_slice(&size.to_be_bytes()[..]);
        self.buffer.extend_from_slice(buf);
    }

    pub async fn read_from_raw_reader<R: AsyncRead + Unpin>(
        &mut self,
        reader: &mut R,
    ) -> io::Result<usize> {
        self.clear();
        let n = reader.read_buf(&mut self.buffer).await?;
        self.set_len(n);
        Ok(n)
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.buffer.extend_from_slice(&0u32.to_be_bytes()[..]);
    }

    fn set_len(&mut self, len: usize) {
        assert!(self.buffer.len() >= len + size_of::<u32>());
        let len: u32 = len.try_into().unwrap();

        self.buffer[..size_of::<u32>()].copy_from_slice(&len.to_be_bytes()[..]);
    }

    pub async fn read_from_ni_reader<'s, R: AsyncRead + Unpin>(
        &'s mut self,
        reader: &'_ mut R,
    ) -> io::Result<&'s [u8]> {
        self.clear();

        reader.read_exact(&mut self.buffer[..]).await?;

        let obj_size = self.get_buffer_len().unwrap();

        self.buffer.reserve(obj_size);

        // SAFETY:
        // - As we just reserve `obj_size` bytes, the following length is valid
        // - ptr comes from a valid memory location
        // - the cast is OK because MaybeUninit<u8> and u8 have the same memory layout
        let buffer = unsafe {
            std::slice::from_raw_parts_mut(
                self.buffer.spare_capacity_mut().as_mut_ptr().cast(),
                obj_size,
            )
        };
        let n = reader.read_exact(buffer).await?;

        // SAFETY:
        // - `n` more are initialized through `read_exact`
        // - `n + size_of::<u32>()` is lesser or equal to buffer capacity by construction
        unsafe {
            self.buffer.set_len(n + size_of::<u32>());
        };

        Ok(self.get_data().unwrap())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct SapRouterHop<'a> {
    pub hostname: Cow<'a, str>,
    pub port: Cow<'a, str>,
    pub password: Cow<'a, str>,
}

impl<'a> Wire<'a> for SapRouterHop<'a> {
    fn encode_into(&self, buffer: &mut Vec<u8>) {
        encode_null_terminated_string(&self.hostname, buffer);
        encode_null_terminated_string(&self.port, buffer);
        encode_null_terminated_string(&self.password, buffer);
    }

    fn decode(input: &'a [u8]) -> nom::IResult<&'a [u8], Self, VerboseError<&'a [u8]>> {
        context(
            "SAP Router hop",
            map(
                tuple((
                    decode_null_terminated_string,
                    decode_null_terminated_string,
                    decode_null_terminated_string,
                )),
                |(hostname, port, password)| Self {
                    hostname,
                    port,
                    password,
                },
            ),
        )(input)
    }
}

impl From<SocketAddrV4> for SapRouterHop<'static> {
    fn from(value: SocketAddrV4) -> Self {
        let port = format!("{}", value.port());
        let hostname = format!("{}", value.ip());

        Self {
            hostname: Cow::Owned(hostname),
            port: Cow::Owned(port),
            password: Cow::Owned(String::new()),
        }
    }
}

impl From<SocketAddrV6> for SapRouterHop<'static> {
    fn from(value: SocketAddrV6) -> Self {
        let port = format!("{}", value.port());
        let hostname = format!("[{}]", value.ip());

        Self {
            hostname: Cow::Owned(hostname),
            port: Cow::Owned(port),
            password: Cow::Owned(String::new()),
        }
    }
}

impl From<SocketAddr> for SapRouterHop<'static> {
    fn from(value: SocketAddr) -> Self {
        match value {
            SocketAddr::V4(ip4) => ip4.into(),
            SocketAddr::V6(ip6) => ip6.into(),
        }
    }
}

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u8)]
pub enum TalkMode {
    MsgIo = 0,
    RawIo = 1,
    RoutIo = 2,
}

impl<'a> Wire<'a> for TalkMode {
    fn encode_into(&self, buffer: &mut Vec<u8>) {
        buffer.push(*self as u8);
    }

    fn decode(input: &'a [u8]) -> nom::IResult<&'a [u8], Self, VerboseError<&'a [u8]>> {
        let (rest, tm) = context("Talk mode", be_u8)(input)?;
        match tm {
            0 => Ok((rest, Self::MsgIo)),
            1 => Ok((rest, Self::RawIo)),
            2 => Ok((rest, Self::RoutIo)),
            _ => Err(nom::Err::Failure(nom::error::make_error(
                input,
                ErrorKind::NoneOf,
            ))),
        }
    }
}

const SAP_ROUTER_VERSION: u8 = 2;

#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SapRouter<'a> {
    Pong,
    Route {
        ni_version: u8,
        talk_mode: TalkMode,
        rest_nodes: u8,
        current_position: usize,
        hops: Vec<SapRouterHop<'a>>,
    },
    ErrorInformation {
        ni_version: u8,
        operation_code: u8,
        return_code: u32,
        text: Cow<'a, str>,
    },
}

const SAP_ROUTER_PONG: &str = "NI_PONG";
const SAP_ROUTER_ROUTE: &str = "NI_ROUTE";
const SAP_ROUTER_ERROR_INFORMATION: &str = "NI_RTERR";

impl<'a> Wire<'a> for SapRouter<'a> {
    fn encode_into(&self, buffer: &mut Vec<u8>) {
        match self {
            SapRouter::Pong => {
                encode_null_terminated_string(SAP_ROUTER_PONG, buffer);
            }
            SapRouter::Route {
                ni_version,
                talk_mode,
                rest_nodes,
                current_position,
                hops,
            } => {
                // type
                encode_null_terminated_string(SAP_ROUTER_ROUTE, buffer);
                // version
                buffer.push(SAP_ROUTER_VERSION);
                // route.ni_version
                buffer.push(*ni_version);
                // route.entries
                buffer.push(hops.len() as u8);
                // route.talk_mode
                talk_mode.encode_into(buffer);
                // unused
                buffer.extend_from_slice(&0u16.to_be_bytes()[..]);
                // rest nodes
                buffer.push(*rest_nodes);

                // route_length
                let route_length_offset = buffer.len();
                buffer.extend_from_slice(&0u32.to_be_bytes()[..]);

                // route_offset (current)
                let route_offset_offset = buffer.len();
                buffer.extend_from_slice(&0u32.to_be_bytes()[..]);
                let mut route_offset = 0u32;

                let route_start_offset = buffer.len();
                for (i, hop) in hops.iter().enumerate() {
                    if i == *current_position {
                        route_offset = (buffer.len() - route_start_offset) as u32;
                    }
                    hop.encode_into(buffer);
                }

                let route_length = (buffer.len() - route_start_offset) as u32;
                (buffer[route_length_offset..][..size_of::<u32>()])
                    .copy_from_slice(&route_length.to_be_bytes()[..]);

                (buffer[route_offset_offset..][..size_of::<u32>()])
                    .copy_from_slice(&route_offset.to_be_bytes()[..]);
            }
            SapRouter::ErrorInformation {
                ni_version,
                operation_code,
                return_code,
                text,
            } => {
                encode_null_terminated_string(SAP_ROUTER_ERROR_INFORMATION, buffer);
                buffer.push(*ni_version);
                buffer.push(*operation_code);
                buffer.extend_from_slice(&return_code.to_be_bytes()[..]);
                let text_size = text.len() as u32;
                buffer.extend_from_slice(&text_size.to_be_bytes()[..]);
                buffer.extend_from_slice(text.as_bytes());
            }
        }
    }

    fn decode(input: &'a [u8]) -> nom::IResult<&'a [u8], Self, VerboseError<&'a [u8]>> {
        let (rest, r#type) = context("SAP Router type", decode_null_terminated_string)(input)?;

        match r#type.as_ref() {
            SAP_ROUTER_PONG => Ok((rest, Self::Pong)),
            SAP_ROUTER_ROUTE => {
                let (
                    rest,
                    (
                        ni_version,
                        entries_count,
                        talk_mode,
                        _padd,
                        rest_nodes,
                        route_length,
                        route_offset,
                    ),
                ) = context(
                    "SAP Router Route",
                    tuple((
                        preceded(verify(be_u8, |&route_info_ver| route_info_ver == 2), be_u8),
                        be_u8,
                        TalkMode::decode,
                        be_u16,
                        be_u8,
                        be_u32,
                        be_u32,
                    )),
                )(rest)?;

                if route_offset > route_length {
                    let e = nom::error::make_error(input, ErrorKind::Verify);
                    let e = ContextError::add_context(
                        input,
                        "route_offset is greater than route_length",
                        e,
                    );
                    return Err(nom::Err::Error(e));
                }

                let (rest, hops_data) = take(route_length as usize)(rest)?;
                let (rest_hops, hops) =
                    many_m_n(1, entries_count as usize, SapRouterHop::decode)(hops_data)?;

                if !rest_hops.is_empty() {
                    eprintln!("Unparsed data in SapRouterHop: {:?}", rest_hops);
                }

                if hops.len() != entries_count as usize {
                    let e = nom::error::make_error(input, ErrorKind::Verify);
                    let e = ContextError::add_context(
                        input,
                        "field `entries_count` does not match hops count",
                        e,
                    );
                    return Err(nom::Err::Error(e));
                }

                let (_, cur) = SapRouterHop::decode(&hops_data[route_offset as usize..])?;
                let Some(current_position) = hops
                    .iter()
                    .enumerate()
                    .find_map(|(i, h)| (h == &cur).then_some(i))
                else {
                    let e = nom::error::make_error(input, ErrorKind::Verify);
                    let e = ContextError::add_context(
                        input,
                        "route_offset does not point to a valid hop",
                        e,
                    );
                    return Err(nom::Err::Error(e));
                };

                Ok((
                    rest,
                    Self::Route {
                        ni_version,
                        rest_nodes,
                        talk_mode,
                        current_position,
                        hops,
                    },
                ))
            }
            SAP_ROUTER_ERROR_INFORMATION => context(
                "SAP Router Error information",
                map(
                    tuple((
                        be_u8,
                        terminated(be_u8, be_u8),
                        be_u32,
                        map_opt(length_data(be_u32), |d| {
                            std::str::from_utf8(d).ok().map(|s| s.into())
                        }),
                    )),
                    |(ni_version, operation_code, return_code, text)| Self::ErrorInformation {
                        ni_version,
                        operation_code,
                        return_code,
                        text,
                    },
                ),
            )(rest),
            _ => Err(nom::Err::Failure(nom::error::make_error(
                input,
                ErrorKind::NoneOf,
            ))),
        }
    }
}

fn decode_null_terminated_string<'i, E>(input: &'i [u8]) -> nom::IResult<&'i [u8], Cow<'i, str>, E>
where
    E: nom::error::ParseError<&'i [u8]> + nom::error::ContextError<&'i [u8]>,
{
    context(
        "Null terminated string",
        terminated(
            map_opt(take_while(|b| b != 0), |d| {
                std::str::from_utf8(d).ok().map(|s| s.into())
            }),
            tag(b"\0"),
        ),
    )(input)
}

fn encode_null_terminated_string(s: &str, buffer: &mut Vec<u8>) {
    buffer.extend_from_slice(s.as_bytes());
    buffer.push(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sap_router_encode() {
        let router = SapRouter::Route {
            talk_mode: TalkMode::RawIo,
            ni_version: 40,
            rest_nodes: 1,
            current_position: 1,
            hops: vec![
                SapRouterHop {
                    hostname: "8.8.8.8".into(),
                    port: "3299".into(),
                    password: "".into(),
                },
                SapRouterHop {
                    hostname: "10.0.0.1".into(),
                    port: "3200".into(),
                    password: "S3cr3t".into(),
                },
            ],
        };
        let mut buffer = Vec::new();
        router.encode_into(&mut buffer);

        assert_eq!(
            &buffer[..],
            b"NI_ROUTE\x00\x02\x28\x02\x01\x00\x00\x01\
                   \x00\x00\x00\x23\x00\x00\x00\x0e\
                   8.8.8.8\x003299\x00\x00\
                   10.0.0.1\x003200\x00S3cr3t\x00"
        );
    }

    #[test]
    fn sap_router_decode() {
        let router = SapRouter::Route {
            talk_mode: TalkMode::RawIo,
            ni_version: 40,
            rest_nodes: 1,
            current_position: 1,
            hops: vec![
                SapRouterHop {
                    hostname: "8.8.8.8".into(),
                    port: "3299".into(),
                    password: "".into(),
                },
                SapRouterHop {
                    hostname: "10.0.0.1".into(),
                    port: "3200".into(),
                    password: "S3cr3t".into(),
                },
            ],
        };
        let input = &b"NI_ROUTE\x00\x02\x28\x02\x01\x00\x00\x01\
                   \x00\x00\x00\x23\x00\x00\x00\x0e\
                   8.8.8.8\x003299\x00\x00\
                   10.0.0.1\x003200\x00S3cr3t\x00"[..];
        let (rest, r) = SapRouter::decode(input).unwrap();
        assert_eq!(rest.len(), 0);
        assert_eq!(r, router);
    }
}
