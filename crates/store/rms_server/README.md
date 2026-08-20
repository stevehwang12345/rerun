# RMS Server

`rms_server` is the local RMS modular-monolith used by the integrated product during development.
It exposes Connect, Projects, Live, Replay, and Live-only Control APIs from one Axum process on `127.0.0.1:8080`.

The default state is an in-memory fixture.
Devices and data sources are organization assets and never contain a project ID.
Projects refer to them through assignments, while every recording owns an immutable project snapshot.

Run the server with:

```powershell
cargo run -p rms_server
```

The fixture Rerun stream endpoints serve the repository's small `spatial3d.rrd` fixture locally.
It is development data and is not a substitute for a growing Redap stream or durable object storage.
