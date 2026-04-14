# Ops — on-call runbook

Placeholders until we have a live deploy. Each section will hold
verified commands.

## Rotate the index from the compressed store

```
# TODO: document once reindex binary exists
```

## Force re-run ingestion verbosely

```
# TODO
```

## Roll back a bad deploy

```
# TODO: systemd unit + previous wheel in /opt/kernel-lore-mcp/previous/
```

## Manually block an IP

```
# nginx deny directive in /etc/nginx/conf.d/deny.conf, then nginx -s reload
```

## Contact points

- Konstantin Ryabitsev (lore/public-inbox maintainer): via
  people.kernel.org, or `Cc: konstantin@linuxfoundation.org`.
- AWS support: per account plan.
- This project: michael@bommaritollc.com.
