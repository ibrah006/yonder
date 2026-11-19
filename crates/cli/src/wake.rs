//! `yonder wake`: wake a sleeping machine with Wake-on-LAN.
//!
//! The signal goes out from this computer and, through Yonder, from every other paired
//! machine that's awake, so it reaches the sleeping one's network even when this
//! computer is somewhere else.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use yonder_core::controller::Controller;

/// How long to wait for the machine to answer after the signal.
const WAIT: Duration = Duration::from_secs(180);

pub async fn run(controller: &Controller, machine: &str, wait: bool) -> Result<()> {
    let target = controller.find_machine(machine)?;
    if target.macs.is_empty() {
        bail!(
            "Yonder doesn't know {}'s network card yet; it learns it the next time {} is online \
             (any command, e.g. `yonder status`)",
            target.name,
            target.name
        );
    }

    // From here, if this computer shares its network.
    for mac in &target.macs {
        let parsed = yonder_core::wake::parse_mac(mac);
        if let Some(parsed) = parsed {
            match tokio::task::spawn_blocking(move || yonder_core::wake::send(parsed)).await? {
                Ok(_) => eprintln!("[yonder] sent the wake signal for {mac} from this computer"),
                Err(err) => eprintln!("[yonder] couldn't send it from this computer: {err}"),
            }
        }
    }

    // From every other machine that's awake.
    let others: Vec<_> = controller
        .machines()
        .into_iter()
        .filter(|m| m.id != target.id)
        .collect();
    let macs = &target.macs;
    let sends = others.iter().map(|via| async move {
        let mut result = Ok(());
        for mac in macs {
            let attempt =
                tokio::time::timeout(Duration::from_secs(8), controller.wake_via(via, mac));
            result = match attempt.await {
                Ok(r) => r.map_err(|e| e.to_string()),
                Err(_) => Err("not reachable".to_string()),
            };
        }
        (via.name.clone(), result)
    });
    for (name, result) in futures_util::future::join_all(sends).await {
        match result {
            Ok(()) => eprintln!("[yonder] {name} sent the wake signal on its network"),
            Err(err) => eprintln!("[yonder] {name} couldn't send it ({err})"),
        }
    }

    if !wait {
        return Ok(());
    }
    eprintln!(
        "[yonder] waiting for {} to answer (up to {} minutes; Ctrl-C to stop)…",
        target.name,
        WAIT.as_secs() / 60
    );
    let started = Instant::now();
    while started.elapsed() < WAIT {
        let ping = tokio::time::timeout(Duration::from_secs(10), controller.ping(&target));
        tokio::select! {
            result = ping => {
                if matches!(result, Ok(Ok(_))) {
                    eprintln!("[yonder] {} is awake ({}s)", target.name, started.elapsed().as_secs());
                    return Ok(());
                }
            }
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    bail!(
        "{} didn't answer within {} minutes. Wake-on-LAN must be on in its firmware (BIOS/UEFI) \
         and network card settings, it usually needs a cable, and the signal must come from its \
         own network (this computer or another machine there)",
        target.name,
        WAIT.as_secs() / 60
    )
}
