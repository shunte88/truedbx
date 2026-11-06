# truedbx

One-shot Dropbox uploader for TrueNAS: uploads a file or folder (recursively) to a Dropbox folder. Missing subfolders are created automatically. Runs as a container job and exits non-zero if any upload fails.

## Setup

1. `cp .env.example .env`, then fill in `TDBX_AKEY` / `TDBX_ASEC` (app key and secret). Credentials are read only from env vars, never command-line flags.
2. Get a refresh token (one time): `docker compose run --rm -it -v ./.env:/env truedbx auth --env-file /env` (or `truedbx auth` on the host, which writes `./.env`). The token is written straight into `.env` with mode 600 and is never printed.

## Run

```bash
docker compose run --rm truedbx                                   # uses TDBX_SOURCE / TDBX_TARGET
docker compose run --rm truedbx --source /data/photos --target /Backups/photos
```

`TDBX_HOST_PATH` is mounted read-only at `/data`, so `--source` must be a path under `/data`. On TrueNAS, schedule the command above as a Cron Job.

| Var | Flag | |
|---|---|---|
| `TDBX_SOURCE` | `-s` | local file/folder (in container) |
| `TDBX_TARGET` | `-t` | Dropbox folder, must start with `/` |
| `TDBX_PARALLEL` | `-p` | concurrent uploads (default 5) |

Files ≥150MB use chunked upload. Hidden files and symlinks are skipped. Existing files are overwritten.

## Image

`.github/workflows/docker.yml` builds and pushes to `ghcr.io/<owner>/<repo>` on pushes to `main` (`latest`, `sha-*`) and on `v*` tags.
