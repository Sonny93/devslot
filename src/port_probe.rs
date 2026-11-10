//! Asking the operating system whether a port is free.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};

use rustix::io::Errno;

use crate::allocation::PortProbe;

const PROBED_ADDRESSES: [IpAddr; 4] = [
    IpAddr::V4(Ipv4Addr::LOCALHOST),
    IpAddr::V4(Ipv4Addr::UNSPECIFIED),
    IpAddr::V6(Ipv6Addr::LOCALHOST),
    IpAddr::V6(Ipv6Addr::UNSPECIFIED),
];

/// A port is free when a TCP `bind` succeeds on `127.0.0.1`, `0.0.0.0`, `::1`
/// and `::`. On the IPv6 addresses, "address not available" and "address
/// family not supported" count as free, for machines where IPv6 is disabled.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemPortProbe;

impl PortProbe for SystemPortProbe {
    fn is_free(&self, port: u16) -> bool {
        PROBED_ADDRESSES
            .iter()
            .all(|address| is_bindable(SocketAddr::new(*address, port)))
    }
}

fn is_bindable(address: SocketAddr) -> bool {
    match TcpListener::bind(address) {
        Ok(_listener) => true,
        Err(error) => is_missing_address_family(&error),
    }
}

fn is_missing_address_family(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::AddrNotAvailable
        || error.raw_os_error().map(Errno::from_raw_os_error) == Some(Errno::AFNOSUPPORT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unused_port() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    #[test]
    fn should_report_free_when_nothing_listens_on_the_port() {
        assert!(SystemPortProbe.is_free(unused_port()));
    }

    #[test]
    fn should_report_busy_when_a_listener_holds_the_port_on_localhost() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();

        assert!(!SystemPortProbe.is_free(port));
    }

    #[test]
    fn should_report_busy_when_a_listener_holds_the_port_on_all_interfaces() {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();

        assert!(!SystemPortProbe.is_free(port));
    }

    #[test]
    fn should_count_an_unavailable_address_as_free_when_the_family_is_missing() {
        let error = std::io::Error::from(std::io::ErrorKind::AddrNotAvailable);

        assert!(is_missing_address_family(&error));
    }

    #[test]
    fn should_not_count_a_busy_port_as_free_when_the_address_is_in_use() {
        let error = std::io::Error::from(std::io::ErrorKind::AddrInUse);

        assert!(!is_missing_address_family(&error));
    }
}
