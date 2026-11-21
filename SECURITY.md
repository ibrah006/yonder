# Security

Yonder runs commands on machines you own, so security problems matter more here than
in most projects.

## Reporting a vulnerability

Please report it privately through GitHub's
[private vulnerability reporting](../../security/advisories/new) for this repository.
Don't open a public issue, and don't share details publicly until a fix is released.

Include what you found, how to reproduce it, and what an attacker could do with it.
You'll get an acknowledgement, and we'll keep you updated until it's fixed.

## Design commitments

- No inbound ports on the machines that run the agent.
- Every command is signed by the controlling device; relays only pass traffic.
- Pairing a new controller requires approval on the machine itself.
- Jobs run without admin rights unless the machine's owner turns that on.
- The agent is always visible on the machine. There is no hidden mode.

## What a paired device can do

Pairing is trust. A device with the default permissions can run jobs and open shells as
the machine's build account (`yonder-build` on Windows). The build account is a standard
user. It can't make itself an administrator, read another device's saved secrets, or
change what the agent records about its jobs. Paired devices share the build account,
though, so a job can see other devices' project copies on the machine. Pair only
devices you would give a shell on that machine.

The agent runs as SYSTEM on Windows and works in folders the build account can also
write to. So it never follows links or junctions inside project copies, never trusts
files a job can change (job settings are read-only to jobs; secret files are removed by
the paths the controller signed), and builds its own upgrades in a folder only SYSTEM
and Administrators can change.

Borrowed git logins (`--borrow-git`) go only to the job that asked for them, over
https, for the project's own remote hosts, while the device that started the job is
following it.
