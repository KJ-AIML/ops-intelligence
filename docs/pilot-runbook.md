# Pilot runbook: capturing real Grafana alerts

For the infra engineer running the first real capture (phase P2). You do not need
to read the code. Every command below is copy-paste. If something here is wrong,
that is a bug in this document; note what happened and keep going.

## 0. What this does and does not do

- It receives every alert your Grafana sends to a new contact point, stores it
  untouched, and lets us replay the week through the engine as often as we want.
- It does not replace, silence, or delay any notification you already receive.
  Grafana keeps delivering to your existing contact points exactly as before.
- Nothing leaves the host you run it on. AI is off. No cloud calls.
- Stopping it is one Grafana click (section 7). Your alerting does not depend on it.

## 1. What you need

- A Linux host with Docker and Docker Compose, 2 vCPU and 4 GB are plenty, that
  Grafana can reach on port 8080, and that you can reach from your workstation.
- Five days where the host stays up. The capture is only as complete as the
  host's uptime, so this should not be a laptop.
- Restrict port 8080 to Grafana's address and your workstation. The product API
  has no login of its own for the pilot (tech sheet 20, option A). Example with ufw:

  ```sh
  sudo ufw allow from <grafana-ip> to any port 8080 proto tcp
  sudo ufw allow from <your-workstation-ip> to any port 8080 proto tcp
  ```

## 2. Start it

```sh
git clone <repo-url> ops-intelligence && cd ops-intelligence
export API_BASE_URL=http://<this-host-ip>:8080     # what the UI prints as the ingestion URL
export POSTGRES_PASSWORD=<pick-a-password>          # anything; it is only reachable on this host
docker compose up -d --build
curl -s localhost:8080/health
```

Expected: `{"status":"ok"}`. Open `http://<this-host-ip>:8080/` from your
workstation. You should see the Overview page with zeros.

Keep the two exported variables somewhere you can find them; use the same values every
time you run a compose command.

## 3. Create the Grafana source

1. Open `http://<this-host-ip>:8080/sources`.
2. Leave the type on "Grafana contact point", type a name such as `grafana-prod`, click Add source.
3. Copy the ingestion URL shown. It is displayed exactly once. If you lose it,
   disable that source and create another.

## 4. Point Grafana at it without touching existing notifications

In Grafana (Alerting → Contact points):

1. New contact point. Name `ops-intelligence`. Integration **Webhook**. URL: the
   ingestion URL from step 3. HTTP method **POST**. Leave "Send resolved" on:
   recoveries are half the data. Set **Max alerts** to `1000`. This is not
   optional: it is what guarantees a fleet-wide storm is truncated with a count
   instead of refused whole. If your alerts are unusually large, the rule is
   `Max alerts` times bytes per alert under 4 MiB with margin.
2. Click **Test** and send the test notification. Back on the Sources page, the
   source's "Last seen" should update within a few seconds. If it does not, the
   host is not reachable from Grafana; check the firewall before anything else.

Then (Alerting → Notification policies):

3. On the default policy, add a **nested policy** and move it to the top.
   Matcher: `alertname =~ .+` (matches everything). Contact point: `ops-intelligence`.
   Enable **Continue matching subsequent sibling nodes**. Save.
4. Trigger or wait for one real alert and confirm you still received it through
   your usual channel. If you did not, delete the nested policy immediately and tell
   the author; do not proceed.

## 5. During the capture (once a day, two minutes)

- Sources page: "Last seen" is recent. If it is more than a few hours old on a
  normal day, check `docker compose ps` and the firewall.
- Overview page: the "failed signals" tile. A non-zero count means some payloads
  could not be understood. Not an emergency: they are stored and will be handled at
  replay. Note the number.
- `docker compose logs server | grep -i truncated`: Grafana drops alerts from very
  large groups; a line here means it happened.

Do not run `process` or `correlate` against the live tenant during the capture.
The capture tenant stays raw; the analysis happens by replay (section 6). This is
what makes the week re-runnable.

## 6. End of capture

Back it up first. The captured week is the asset; the code is replaceable.

```sh
docker compose exec postgres pg_dump -U ops -Fc ops_intelligence > capture-$(date +%F).dump
```

Copy that file somewhere that is not this host.

Freeze the week into a dataset, replay it twice, and confirm the two runs agree:

```sh
docker compose run --rm worker pilot dataset create week-01 grafana-prod
docker compose run --rm worker pilot replay week-01 run-a
docker compose run --rm worker pilot replay week-01 run-b
docker compose run --rm worker pilot compare run-a run-b
```

Expected: both replays print the same counts, and compare reports no deltas.
Each replay prints a `ui slug` line such as `replay-4f2c…`. To look at a replay in
the UI, restart the server pointed at that tenant, then point it back afterwards:

```sh
DEFAULT_ORGANIZATION_SLUG=replay-<id> docker compose up -d server
# ... review at http://<this-host-ip>:8080/ ...
docker compose up -d server                     # back to the capture tenant
```

## 7. Stop or roll back

- Grafana: delete the nested policy (section 4 step 3). Notifications to the
  engine stop instantly; nothing else changes.
- Host: `docker compose down`. Data stays in the `ops-pgdata` volume. To remove
  it entirely: `docker compose down -v`, after the backup in section 6.
- To pause without removing: disable the source on the Sources page. Grafana
  will get a 400 and give up after its retries.

## 8. The review session (one hour, author present)

Walk every incident in run-a together and answer, per incident: right grouping,
wrong grouping, or missed grouping. Then the seven questions from the product
sheet (section 31, Day 7):

1. What did this show you that your existing tools did not?
2. What did it hide that you still needed?
3. Which grouping was wrong?
4. Which insight was useless?
5. What should be automatic next?
6. Would you keep this running tomorrow?
7. If it disappeared tomorrow, would you care?

Record the answers in `docs/pilot-01-evaluation.md`. Your corrections become the
golden cases for phase P3.

## 9. Known ceilings

- Request bodies over 4 MiB are refused whole and logged as
  `request body exceeds the ingest limit`. With `Max alerts` set as in section 4
  this cannot happen; if the line appears anyway, tell the author the same day.
- A group truncated by `Max alerts` is logged as
  `grafana dropped alerts from this notification group`, with the count. That is
  the expected behaviour in a large storm, not a fault; note the count for the
  review session.
- A batch whose group context multiplied across its alerts would pass 32 MiB is
  refused and logged as `grafana batch rejected`. At real Grafana shapes this is
  unreachable. If the line appears, tell the author: the notification was shaped
  unlike anything the engine expects.
- A generic-webhook body the engine cannot read is refused and logged as
  `webhook payload rejected`. Same instruction.
- Which alert labels mean environment, service and resource are fixed guesses
  (`environment`/`env`, `service`/`job`/`app`, `instance`/`host`/`resource`/`pod`).
  Real data will correct them; that is expected.
- Severity comes from a label named `severity`. Alerts without it are treated as
  warning while firing.
- Everything the engine computes is deterministic and re-runnable. Nothing you do
  in the UI changes the captured signals.
