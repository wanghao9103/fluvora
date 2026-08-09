# Fluvora operations console

The shipped operations console is the provisioned Grafana `Fluvora Overview` dashboard plus the
status API at `/v1/status`. Compose exposes Grafana on port 3000, Prometheus on 9090 and
Alertmanager on 9093. Dashboard and datasource definitions live in `deploy/monitoring/grafana`.
