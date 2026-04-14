# Ingestion — grokmirror

Lore publishes a `manifest.js.gz` with a per-shard fingerprint. We
don't re-invent that; we use `grokmirror` which is what
lore.kernel.org documents as the canonical mirror tool.

## Setup on the deploy box

```bash
sudo apt install grokmirror   # Debian 13; lags a bit — pip install for latest
pip install grokmirror         # preferred

mkdir -p /var/lore-mirror
```

`/etc/grokmirror/lore.conf`:
```ini
[core]
toplevel = /var/lore-mirror
log = /var/log/grokmirror/lore.log
manifest = /var/lore-mirror/manifest.js.gz

[remote]
site = https://lore.kernel.org
manifest = https://lore.kernel.org/manifest.js.gz

[pull]
projectslist =
# empty = pull all
pull_threads = 4
include = /*
purgeprotect = 2
```

## Cron

```
# /etc/cron.d/lore-mirror
*/10 * * * * root grok-pull -c /etc/grokmirror/lore.conf
```

## Subsystem trees (separate)

Chuck Lever, Steve French, Namjae Jeon, linux-next, etc. — mirror
in a sibling dir with its own grokmirror config. These are
*source* trees, not mailing lists, and our Rust code treats them
differently (commit-based history with no `m` blob convention).

## First pull

Cold clone of all of lore is 50–120 GB of git objects and takes
hours depending on network. Run it once manually with
`grok-pull -c /etc/grokmirror/lore.conf -vv` before enabling the
cron.

## What we watch

- `/var/lore-mirror/manifest.js.gz` mtime — last successful update.
- Per-shard fingerprint deltas (our ingestor tracks these and
  skips unchanged shards).
- Disk usage on `/var/lore-mirror` partition.
