//! `yonder footprint`, `clean`, `unpair`, `devices`, `revoke`: what this device left on
//! machines, removing it, and cutting off a lost device.

use anyhow::{Result, bail};
use yonder_core::controller::Controller;
use yonder_core::footprint::{DiskEncryption, Footprint, FootprintItem, Removed};
use yonder_core::secrets::SecretName;
use yonder_core::store::{PairedMachine, format_date, format_datetime_ms};
use yonder_core::workspace::WorkspaceId;

use crate::workspace::human_bytes;

/// The machines a command is for: one, or every paired machine for `all`.
async fn targets(controller: &Controller, machine: &str) -> Result<Vec<PairedMachine>> {
    if machine == "all" {
        let machines = controller.machines();
        if machines.is_empty() {
            bail!("no paired machines");
        }
        return Ok(machines);
    }
    Ok(vec![controller.pick_machine(machine).await?])
}

pub async fn show(controller: &Controller, machine: Option<String>) -> Result<()> {
    let machines = targets(controller, machine.as_deref().unwrap_or("all")).await?;
    for (i, machine) in machines.iter().enumerate() {
        if i > 0 {
            println!();
        }
        match controller.footprint(machine).await {
            Ok(fp) => print_footprint(machine, &fp),
            Err(err) => println!("{}: couldn't ask ({err})", machine.name),
        }
    }
    Ok(())
}

fn print_footprint(machine: &PairedMachine, fp: &Footprint) {
    println!(
        "{} (paired {}; {})",
        machine.name,
        format_date(fp.paired_at),
        fp.permissions
    );
    println!(
        "  disk encryption: {}",
        match fp.disk_encryption {
            DiskEncryption::On => "on",
            DiskEncryption::Off => "OFF (secrets and project copies sit unencrypted on its disk)",
            DiskEncryption::Unknown => "unknown",
        }
    );
    println!(
        "  auto-cleanup: {}",
        match fp.cleanup_after_days {
            Some(days) => format!("project copies unused for {days} days are removed"),
            None => format!("off (turn on: yonder cleanup {} --after 30d)", machine.name),
        }
    );
    if fp.is_empty() {
        println!("  nothing of this device's is left here");
        return;
    }
    println!("  {} on disk", human_bytes(fp.bytes()));
    for s in &fp.secrets {
        println!(
            "  secret     {:28} {}{}",
            s.name.as_str(),
            s.last_used_ms
                .map(|t| format!("last used {}", format_datetime_ms(t)))
                .unwrap_or_else(|| "never used".into()),
            s.note
                .as_deref()
                .map(|n| format!(" — {n}"))
                .unwrap_or_default()
        );
    }
    for w in &fp.workspaces {
        println!(
            "  project    {:28} {} in {} files{}",
            w.id.as_str(),
            human_bytes(w.bytes),
            w.files,
            w.last_synced_ms
                .map(|t| format!(", synced {}", format_datetime_ms(t)))
                .unwrap_or_default()
        );
    }
    if fp.jobs > 0 {
        let unfinished = if fp.unfinished_jobs > 0 {
            format!(", {} unfinished", fp.unfinished_jobs)
        } else {
            String::new()
        };
        println!(
            "  jobs       {:28} {} of logs",
            format!("{}{unfinished}", fp.jobs),
            human_bytes(fp.job_bytes)
        );
    }
    for t in &fp.shells {
        println!(
            "  shell      {:28} started {}",
            t.id.as_str(),
            format_datetime_ms(t.created_at_ms)
        );
    }
    println!(
        "  Remove with: yonder clean {} everything   (or jobs, shells, project, secret:NAME, workspace:ID)",
        machine.name
    );
}

fn parse_item(text: &str, here: Option<&WorkspaceId>) -> Result<FootprintItem> {
    Ok(match text {
        "everything" | "all" => FootprintItem::Everything,
        "jobs" => FootprintItem::Jobs,
        "shells" => FootprintItem::Shells,
        "project" => match here {
            Some(id) => FootprintItem::Workspace(id.clone()),
            None => bail!("`project` means the copy of the git project you're in; run it from one"),
        },
        _ => match text.split_once(':') {
            Some(("secret", name)) => match SecretName::parse(name) {
                Some(name) => FootprintItem::Secret(name),
                None => bail!("\"{name}\" isn't a secret name"),
            },
            Some(("workspace" | "project", id)) => match WorkspaceId::parse(id) {
                Some(id) => FootprintItem::Workspace(id),
                None => bail!("\"{id}\" isn't a project copy id (see `yonder footprint`)"),
            },
            _ => bail!(
                "don't know \"{text}\"; use everything, jobs, shells, project, secret:NAME or workspace:ID"
            ),
        },
    })
}

/// The workspace id of the git project around the current folder, if there is one.
fn project_here(controller: &Controller) -> Option<WorkspaceId> {
    let here = std::env::current_dir().ok()?;
    let root = crate::workspace::git_root(&here).ok()?;
    Some(WorkspaceId::for_project(&root, &controller.id()))
}

fn print_removed(machine: &PairedMachine, removed: &Removed) {
    let mut parts = Vec::new();
    for (count, what) in [
        (removed.secrets, "secrets"),
        (removed.workspaces, "project copies"),
        (removed.jobs, "jobs"),
        (removed.shells, "shells"),
    ] {
        if count > 0 {
            parts.push(format!("{count} {what}"));
        }
    }
    if parts.is_empty() {
        println!("{}: nothing to remove", machine.name);
    } else {
        println!(
            "{}: removed {} ({} freed)",
            machine.name,
            parts.join(", "),
            human_bytes(removed.bytes)
        );
    }
    for note in &removed.kept {
        println!("  kept: {note}");
    }
}

pub async fn clean(controller: &Controller, machine: &str, what: &[String]) -> Result<()> {
    let here = project_here(controller);
    let items = what
        .iter()
        .map(|w| parse_item(w, here.as_ref()))
        .collect::<Result<Vec<_>>>()?;
    for machine in targets(controller, machine).await? {
        let mut total = Removed::default();
        for item in &items {
            match controller.remove_footprint(&machine, item.clone()).await {
                Ok(removed) => total.add(removed),
                Err(err) => {
                    println!("{}: {err}", machine.name);
                    break;
                }
            }
        }
        print_removed(&machine, &total);
    }
    Ok(())
}

/// `yonder cleanup`: turn this device's auto-cleanup on or off, or show it.
pub async fn cleanup(
    controller: &Controller,
    machine: &str,
    after: Option<String>,
    off: bool,
) -> Result<()> {
    let setting = match (after, off) {
        (Some(_), true) => bail!("use --after or --off, not both"),
        (Some(text), false) => Some(parse_days(&text)?),
        (None, true) => None,
        (None, false) => {
            for machine in targets(controller, machine).await? {
                match controller.footprint(&machine).await {
                    Ok(fp) => match fp.cleanup_after_days {
                        Some(days) => println!(
                            "{}: project copies unused for {days} days are removed",
                            machine.name
                        ),
                        None => println!("{}: auto-cleanup is off", machine.name),
                    },
                    Err(err) => println!("{}: couldn't ask ({err})", machine.name),
                }
            }
            return Ok(());
        }
    };
    for machine in targets(controller, machine).await? {
        match controller.set_cleanup(&machine, setting).await {
            Ok(()) => match setting {
                Some(days) => println!(
                    "{}: this device's project copies unused for {days} days will be removed (checked hourly)",
                    machine.name
                ),
                None => println!("{}: auto-cleanup is off", machine.name),
            },
            Err(err) => println!("{}: {err}", machine.name),
        }
    }
    Ok(())
}

/// `30d`, `2w` or a plain number of days.
fn parse_days(text: &str) -> Result<u32> {
    let text = text.trim();
    let (number, per) = if let Some(n) = text.strip_suffix('w') {
        (n, 7)
    } else {
        (text.strip_suffix('d').unwrap_or(text), 1)
    };
    match number.parse::<u32>() {
        Ok(n) if n > 0 => Ok(n * per),
        _ => bail!("\"{text}\" isn't a number of days; use e.g. 30d or 2w"),
    }
}

pub async fn unpair(controller: &Controller, machine: &str, keep: bool) -> Result<()> {
    for machine in targets(controller, machine).await? {
        match controller.unpair(&machine, !keep).await {
            Ok(removed) => {
                print_removed(&machine, &removed);
                println!(
                    "{}: unpaired; this device can't reach it any more.",
                    machine.name
                );
            }
            Err(err) => println!("{}: {err}", machine.name),
        }
    }
    Ok(())
}

pub async fn devices(controller: &Controller, machine: &str) -> Result<()> {
    let machine = controller.pick_machine(machine).await?;
    let devices = controller.devices(&machine).await?;
    println!("{:20}  {:12}  {:10}  PERMISSIONS", "DEVICE", "ID", "PAIRED");
    for d in devices {
        println!(
            "{:20}  {:12}  {:10}  {}{}",
            d.name,
            d.id.fmt_short().to_string(),
            format_date(d.paired_at),
            d.permissions,
            if d.you { "  (this device)" } else { "" }
        );
    }
    Ok(())
}

pub async fn revoke(controller: &Controller, device: &str, on: &str, keep: bool) -> Result<()> {
    let query = device.trim().to_lowercase();
    let mut found_any = false;
    for machine in targets(controller, on).await? {
        let devices = match controller.devices(&machine).await {
            Ok(devices) => devices,
            Err(err) => {
                println!("{}: couldn't ask ({err})", machine.name);
                continue;
            }
        };
        let matches: Vec<_> = devices
            .iter()
            .filter(|d| {
                !d.you && (d.name.to_lowercase() == query || d.id.to_string().starts_with(&query))
            })
            .collect();
        match matches.as_slice() {
            [] => println!("{}: no device \"{device}\" paired", machine.name),
            [one] => {
                found_any = true;
                match controller.revoke_device(&machine, one.id, !keep).await {
                    Ok(removed) => {
                        println!("{}: revoked {}", machine.name, one.name);
                        print_removed(&machine, &removed);
                    }
                    Err(err) => println!("{}: {err}", machine.name),
                }
            }
            _ => println!(
                "{}: several devices match \"{device}\"; use more of the id (`yonder devices {}`)",
                machine.name, machine.name
            ),
        }
    }
    if !found_any {
        bail!("\"{device}\" wasn't revoked anywhere");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_parse() {
        assert_eq!(parse_days("30d").unwrap(), 30);
        assert_eq!(parse_days("2w").unwrap(), 14);
        assert_eq!(parse_days("7").unwrap(), 7);
        for bad in ["0d", "", "x", "-1d", "3h"] {
            assert!(parse_days(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn items_parse() {
        let here = WorkspaceId::parse("app-1234abcd").unwrap();
        assert_eq!(
            parse_item("everything", None).unwrap(),
            FootprintItem::Everything
        );
        assert_eq!(
            parse_item("project", Some(&here)).unwrap(),
            FootprintItem::Workspace(here.clone())
        );
        assert!(parse_item("project", None).is_err());
        assert_eq!(
            parse_item("secret:TOKEN", None).unwrap(),
            FootprintItem::Secret(SecretName::parse("TOKEN").unwrap())
        );
        assert_eq!(
            parse_item("workspace:app-1234abcd", None).unwrap(),
            FootprintItem::Workspace(here)
        );
        for bad in ["secret:../x", "workspace:..", "stuff"] {
            assert!(parse_item(bad, None).is_err(), "{bad}");
        }
    }
}
