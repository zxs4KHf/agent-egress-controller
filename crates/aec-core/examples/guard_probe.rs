//! A single scoped test connection, no credentials, proxy changes or other side effects.
fn main() {
    use std::time::Duration;
    let args: Vec<_> = std::env::args().skip(1).collect();
    let address = args[1].parse::<std::net::SocketAddr>().unwrap();
    let ok = if args[0] == "tcp" {
        std::net::TcpStream::connect_timeout(&address, Duration::from_millis(800)).is_ok()
    } else {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
        socket.connect(address).is_ok()
            && socket.send(b"probe").is_ok()
            && socket.recv(&mut [0; 16]).is_ok()
    };
    std::process::exit(if ok { 0 } else { 1 });
}
