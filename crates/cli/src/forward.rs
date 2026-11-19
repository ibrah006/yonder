//! `yonder forward`: a port on a machine, reachable on this computer's localhost.
//!
//! Each connection to the local port becomes a stream to the machine, which connects to
//! the port there. Traffic goes over Yonder's encrypted connection, so the port doesn't
//! have to be open on the machine's network.

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use tokio::io::AsyncWriteExt;
use yonder_core::controller::Controller;
use yonder_core::store::PairedMachine;

/// `3000` or `host:3000`, as the machine sees it.
pub fn parse_target(text: &str) -> Result<(String, u16)> {
    let (host, port) = match text.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => (host.trim_matches(['[', ']']).to_string(), port),
        _ => ("127.0.0.1".to_string(), text),
    };
    match port.parse::<u16>() {
        Ok(port) if port > 0 => Ok((host, port)),
        _ => bail!("\"{text}\" isn't a port; use e.g. 3000 or 127.0.0.1:3000"),
    }
}

pub async fn run(
    controller: &Controller,
    machine: &str,
    target: &str,
    local_port: Option<u16>,
) -> Result<()> {
    let (host, port) = parse_target(target)?;
    let machine = controller.pick_machine(machine).await?;
    let local_port = local_port.unwrap_or(port);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", local_port))
        .await
        .with_context(|| {
            format!("couldn't listen on localhost:{local_port}; is something using it? Pick another with --local")
        })?;
    // Fail early, and clearly, if nothing listens there.
    drop(controller.forward(&machine, &host, port).await?);
    eprintln!(
        "[yonder] localhost:{local_port} → {host}:{port} on {}. Ctrl-C to stop.",
        machine.name
    );

    let mut connections = FuturesUnordered::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (socket, _) = accepted?;
                let _ = socket.set_nodelay(true);
                connections.push(pass(controller, &machine, &host, port, socket));
            }
            Some(()) = connections.next(), if !connections.is_empty() => {}
            _ = tokio::signal::ctrl_c() => {
                eprintln!();
                eprintln!("[yonder] stopped forwarding");
                return Ok(());
            }
        }
    }
}

/// One local connection, passed to the machine and back.
async fn pass(
    controller: &Controller,
    machine: &PairedMachine,
    host: &str,
    port: u16,
    socket: tokio::net::TcpStream,
) {
    let (mut send, mut recv) = match controller.forward(machine, host, port).await {
        Ok(halves) => halves,
        Err(err) => {
            eprintln!("[yonder] couldn't forward a connection: {err}");
            return;
        }
    };
    let (mut local_read, mut local_write) = socket.into_split();
    let up = async {
        let _ = tokio::io::copy(&mut local_read, &mut send).await;
        let _ = send.shutdown().await;
    };
    let down = async {
        let _ = tokio::io::copy(&mut recv, &mut local_write).await;
        let _ = local_write.shutdown().await;
    };
    tokio::join!(up, down);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_parse() {
        assert_eq!(parse_target("3000").unwrap(), ("127.0.0.1".into(), 3000));
        assert_eq!(
            parse_target("localhost:8080").unwrap(),
            ("localhost".into(), 8080)
        );
        assert_eq!(
            parse_target("192.168.1.5:22").unwrap(),
            ("192.168.1.5".into(), 22)
        );
        assert_eq!(parse_target("[::1]:5000").unwrap(), ("::1".into(), 5000));
        for bad in ["", "x", "0", "70000", "host:"] {
            assert!(parse_target(bad).is_err(), "{bad}");
        }
    }
}
