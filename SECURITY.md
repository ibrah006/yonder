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
