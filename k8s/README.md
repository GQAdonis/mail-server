# Stalwart on GKE (Kustomize)

Deploys Stalwart mail & collaboration server to a GKE cluster with:

- **Envoy Gateway** (Gateway API) exposing web/JMAP (HTTPS) and all mail protocols (raw TCP)
- **cert-manager + Let's Encrypt** (ACME HTTP-01 via the Gateway API) for `mail.parlour-world.io`
- **PostgreSQL** as the data + full-text store (in-cluster StatefulSet)
- **Google Cloud Storage** as the blob store (S3 interop)
- **Immediate-binding** persistent volumes

```
k8s/
├── base/                      # reusable manifests + placeholder secrets
│   ├── storageclass-immediate.yaml
│   ├── postgres/              # StatefulSet + headless Service
│   ├── stalwart/              # StatefulSet + Service + bootstrap config.json
│   └── gateway/               # GatewayClass, Gateway, HTTP/TCP routes, cert-manager
└── overlays/parlour-world/    # env-specific: image tag, bucket, real secrets
```

> [!IMPORTANT]
> ## Stalwart v0.16 changed how it is configured
> As of **v0.16**, Stalwart no longer uses TOML config files. A single JSON file
> (mounted at `/etc/stalwart/config.json`) tells the server only **where its data
> store lives**. **Everything else** — domains, DKIM keys, listener tuning, spam
> filter, TLS sourcing for the mail protocols — lives in the database and is
> configured **after first boot** through the web admin UI or the `stalwart`
> CLI's `config apply`. This project provisions the infrastructure and the
> bootstrap file; it does **not** (and cannot) fully configure the mail domain
> declaratively. See the two-phase workflow below.

---

## Prerequisites

Install these cluster add-ons once (they are not part of this Kustomize project):

```bash
# Gateway API CRDs (EXPERIMENTAL channel — required for TLSRoute + TCPRoute,
# which the standard channel does not include) ...
kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.4.0/experimental-install.yaml

# ... then Envoy Gateway, with TLSRoute/TCPRoute enabled in its EnvoyGateway
# config (extensionApis / runtime flags). Envoy Gateway ships these behind a
# config switch; see its "Customize EnvoyGateway" docs.
helm install eg oci://docker.io/envoyproxy/gateway-helm \
  --version v1.8.0 -n envoy-gateway-system --create-namespace

# cert-manager WITH the Gateway API feature enabled
helm repo add jetstack https://charts.jetstack.io
helm upgrade --install cert-manager jetstack/cert-manager \
  -n cert-manager --create-namespace \
  --set crds.enabled=true \
  --set config.apiVersion=controller.config.cert-manager.io/v1alpha1 \
  --set config.kind=ControllerConfiguration \
  --set config.enableGatewayAPI=true
```

You also need, outside the cluster:

- A **GCS bucket** (e.g. `parlour-world-mail-blobs`) and **HMAC interop keys** for a
  service account (Cloud Console → Cloud Storage → Settings → Interoperability).
- DNS control of `parlour-world.io` to create the `A` and `MX` records (step 4).

## Configure the overlay

1. **Secrets** — copy the example env files and fill in real values:
   ```bash
   cd k8s/overlays/parlour-world/secrets
   cp postgres.env.example postgres.env
   cp runtime.env.example  runtime.env
   # edit both; STALWART_PG_PASSWORD must equal POSTGRES_PASSWORD
   ```
   These are gitignored. For GitOps, swap the `secretGenerator`s for an
   `ExternalSecret` / SealedSecret instead.

2. **Bootstrap config** — edit `overlays/parlour-world/config-bootstrap.json`:
   set `blobStore.bucket` and `blobStore.accessKey` (the GCS HMAC **access key id**;
   the secret key comes from the `stalwart-runtime` Secret via env).

3. **Issuer email** — set a real contact in `base/gateway/certificate.yaml`
   (`ClusterIssuer.spec.acme.email`).

4. **Image tag** — pin a digest in `overlays/parlour-world/kustomization.yaml`
   (`images[0].newTag`) instead of a floating tag for production.

## Deploy

```bash
# Render and review
kustomize build k8s/overlays/parlour-world | less

# Apply
kubectl apply -k k8s/overlays/parlour-world
```

### Phase 1 — infrastructure comes up

Postgres and Stalwart start; Envoy Gateway provisions a LoadBalancer with one
external IP carrying every listener (443 + the mail ports). The overlay sets a
**fallback admin** (`STALWART_RECOVERY_ADMIN=admin:<password>` in the
`stalwart-runtime` secret) so you can log in to the web UI on :443 before any
real admin account exists — this is honored at normal auth time, so the server
runs normally (not in recovery mode). cert-manager issues the cert once DNS
resolves (step 4).

```bash
# Get the external IP
kubectl -n stalwart get gateway stalwart-gateway -o jsonpath='{.status.addresses[0].value}'; echo
```

### Phase 2 — point DNS, then finish setup

1. Point DNS at the external IP from above. A specific `mail` A record overrides
   any wildcard. Keep it **unproxied** (DNS-only) — the mail ports are raw TCP
   that an HTTP proxy can't carry, and ACME HTTP-01 must reach the gateway
   directly:
   - `mail.parlour-world.io  A     <EXTERNAL_IP>`   (proxied = false)
   - `parlour-world.io       MX 10 mail.parlour-world.io`
   - SPF/DKIM/DMARC `TXT` records (Stalwart generates DKIM keys on first boot —
     read the selector from the admin UI).

   Cloudflare example (zone hosted on Cloudflare):
   ```bash
   curl -X POST "https://api.cloudflare.com/client/v4/zones/<ZONE_ID>/dns_records" \
     -H "Authorization: Bearer $CF_TOKEN" -H "Content-Type: application/json" \
     -d '{"type":"A","name":"mail","content":"<EXTERNAL_IP>","ttl":300,"proxied":false}'
   ```
2. Wait for the cert: `kubectl -n stalwart get certificate stalwart-tls`
   (should report `Ready=True` once the A record resolves).
3. Open **https://mail.parlour-world.io/** and log in as `admin` with the
   fallback password. In the admin UI:
   - create the `parlour-world.io` domain and a real admin account;
   - point Stalwart's **TLS** (web/:443 + mail protocols) at the mounted cert
     `/etc/stalwart/tls/tls.crt` + `tls.key` (populated by cert-manager) — until
     then Stalwart serves a self-signed `rcgen` cert;
   - configure the blob store (this overlay defaults to a FileSystem blob store
     on the data PVC; switch to S3/GCS here once HMAC interop keys exist).
4. **After** a real admin account exists, remove the fallback admin: delete the
   `STALWART_RECOVERY_ADMIN` patch from the overlay + the secret key, re-apply,
   and roll the pod.

## How traffic flows

> [!NOTE]
> After changing a Gateway listener's protocol (e.g. swapping the :443 route
> kind), restart the Envoy proxy pod for this gateway so it fully reconciles —
> stale xDS config otherwise resets connections (`bytes_sent: 0`):
> `kubectl delete pod -n envoy-gateway-system -l gateway.envoyproxy.io/owning-gateway-name=stalwart-gateway`

| Port(s) | Listener | Route kind | TLS terminated by |
|---|---|---|---|
| 443 | `https` (TCP) | `TCPRoute` | **Stalwart** |
| 80  | `http` | `HTTPRoute` (ACME + redirect) | n/a |
| 25, 465, 587, 143, 993, 110, 995, 4190 | per-port `TCP` | `TCPRoute` | **Stalwart** (L4 passthrough) |
| 8080 | none (port-forward only) | — | n/a |

Envoy stays pure L4 for everything: Stalwart terminates TLS for web/JMAP (:443)
and the mail protocols itself (it implements STARTTLS / implicit TLS and the
SMTP/IMAP/POP3 state machines), so there is a single TLS termination point and no
re-encrypt / BackendTLSPolicy to manage. The cert-manager cert is mounted into
the pod, not referenced by the Gateway.

## Notes & caveats

- **SMTP on :25** — GCP allows inbound :25 to LoadBalancers; outbound :25 from GCE
  is throttled, which affects *sending*. Use a smarthost/relay on 587 if you hit
  delivery limits.
- **Single replica** — Postgres and Stalwart are 1 replica each. Stalwart v0.16
  supports clustering via `STALWART_ROLE` + the in-DB config; scale out only after
  the in-DB cluster settings are configured.
- **Immediate-binding storage** — `stalwart-immediate` uses
  `volumeBindingMode: Immediate` with `reclaimPolicy: Retain`. PVs are kept on
  delete; clean them up manually if you tear down.
- **Backups** — not included. Back up the Postgres volume and the GCS bucket
  separately.

## Validate locally (no cluster)

```bash
kustomize build k8s/base                     # base renders standalone
kustomize build k8s/overlays/parlour-world   # full env
```
