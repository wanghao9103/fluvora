# Media processing

Non-interactive media capabilities:

- `media-pipeline`: bounded FFmpeg/HLS/CMAF process specifications.
- `media-store`: media object persistence.
- `transcode-bridge`: codec negotiation and transcoding decisions.

These crates expose capabilities; workers and gateways compose them from
`../services/`.
