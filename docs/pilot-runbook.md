# Pilot runbook: capturing real Grafana alerts

For the infra engineer running the first real capture (phase P2). You do not need
to read the code. Nearly every command below is copy-paste as written: wiring
Grafana's own contact point (section 4) has to happen in Grafana's web UI, there
is no way around that, but creating this product's source (section 3) can be
done either through its web UI or a terminal command — take whichever you
prefer. If something here is wrong, that is a bug in this document; note what
happened and keep going.

Anywhere a command below contains `REPLACE_WITH_...`, that is a literal
placeholder, not a real value — swap it out before running. It is written that
way, in capitals with no punctuation, on purpose: the more familiar
`<like-this>` style looks like a placeholder to a human but is not one to bash,
which reads a bare `<` as "read input from a file named like-this" and fails
with a confusing `No such file or directory` instead of telling you what is
actually wrong. `REPLACE_WITH_...` either fails just as loudly for a reason
that is obvious, or runs and then plainly does not work — either way it points
you back here instead of doing something silently wrong.

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
  sudo ufw allow from REPLACE_WITH_GRAFANA_IP to any port 8080 proto tcp
  sudo ufw allow from REPLACE_WITH_YOUR_WORKSTATION_IP to any port 8080 proto tcp
  ```

## 2. Start it

Check whether this host already has a deployment before touching anything:

```sh
docker compose ps
```

- If that command fails (no such file, no checkout yet) or its output is just
  the header row with nothing under it, this is a fresh start — continue with
  "First time on this host" below.
- If it lists containers (`postgres`, `server`), skip straight to "Already
  running" below. Do not clone on top of an existing checkout or run
  `--build` without reading that section first — a capture may be in progress.

### First time on this host

```sh
git clone REPLACE_WITH_REPO_URL ops-intelligence && cd ops-intelligence   # the author gives you this URL at handoff
export THIS_HOST_IP=REPLACE_WITH_THIS_HOST_IP   # the address Grafana and your workstation use to reach this host — the same one allowed through the firewall in section 1; find it with: hostname -I
export API_BASE_URL=http://$THIS_HOST_IP:8080   # baked into the ingestion URL the UI (and the curl equivalent in section 3) will show you later — set it now, before either exists
export POSTGRES_PASSWORD=REPLACE_WITH_A_PASSWORD   # anything; it is only reachable on this host
docker compose up -d --build
curl -s localhost:8080/health
```

Expected: `{"status":"ok"}`. From your workstation, open `http://` followed by
your real host address and `:8080/` (not the literal text `$THIS_HOST_IP`).
You should see the Overview page with zeros.

Write down the value you used for `THIS_HOST_IP` and the password you picked;
you need to `export` them again in any new shell before running a compose
command — nothing persists them for you.

### Already running

The stack came up before — a reboot, or you are back for section 5 or 6.
Re-export `THIS_HOST_IP`, `API_BASE_URL` and `POSTGRES_PASSWORD` in this shell
using the same values as when you first ran "First time on this host", then:

- To confirm it is healthy without changing anything: `docker compose ps` —
  both containers should say `healthy`. That is normally all you need here.
- Only if the author sent you a new checkout to pick up: `docker compose up -d
  --build`. This recreates the `server` container, so ingestion is unavailable
  for the few seconds that takes; Grafana retries and nothing is lost, exactly
  as in section 5. It does not touch anything stored in the `ops-pgdata`
  volume.
- Do not run `docker compose down` or `docker compose down -v` here — see
  section 7 for what those actually do and when they belong.

## 3. Create the Grafana source

Pick one of the two ways below. Either way, you choose a name for this source —
write it down, verbatim; section 6 needs that exact name again, days from now.

**Web UI:**

1. Open `http://` followed by your host address and `:8080/sources`.
2. Leave the type on "Grafana contact point", type a name such as `grafana-prod`, click Add source.
3. Copy the ingestion URL shown. It is displayed exactly once. If you lose it,
   disable that source and create another.

**Terminal, if you would rather not open the product's UI yet:**

```sh
curl -s -i -X POST http://$THIS_HOST_IP:8080/api/v1/sources \
  -H 'content-type: application/json' \
  -d '{"name":"REPLACE_WITH_A_SOURCE_NAME","source_type":"grafana"}'
```

Expected: an `HTTP/1.1 201 Created` status line (that is what `-i` is for), and
a JSON body containing an `ingest_url` field — that is the ingestion URL for
section 4. Like the web UI, this is the only time the URL and its token are
shown; if you lose it, disable the source on the Sources page and create
another.

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
docker compose exec postgres pg_dump -U ops -Fc ops_intelligence > ~/capture-$(date +%F).dump
```

That writes the dump to your home directory, not the `ops-intelligence`
checkout you `cd`'d into in section 2 — keep it outside the checkout so a
copy-paste never drops a database dump into the source tree. If you are
already somewhere else, use that path instead; the point is "not inside the
checkout", not the specific directory.

Copy that file somewhere that is not this host.

Freeze the week into a dataset. Use the exact source name you wrote down in
section 3 in place of `REPLACE_WITH_THE_SOURCE_NAME_FROM_SECTION_3` below:

```sh
docker compose run --rm worker pilot dataset create week-01 REPLACE_WITH_THE_SOURCE_NAME_FROM_SECTION_3
docker compose run --rm worker pilot dataset list
```

Check the `week-01` row in that listing: `SIGNALS` should look like a real
week of alerts, not a handful. If the source named in section 3 never actually
received any traffic — wrong name, or created but never wired into Grafana —
`dataset create` refuses outright with `no captured signals matched; dataset
not created` rather than silently building an empty dataset. Either way, sort
that out here; do not continue to the replay below on a dataset you have not
confirmed.

Replay it twice and confirm the two runs agree:

```sh
docker compose run --rm worker pilot replay week-01 run-a
docker compose run --rm worker pilot replay week-01 run-b
docker compose run --rm worker pilot compare run-a run-b
```

Expected: both replays print the same counts, and compare reports no deltas.
Each replay also prints a `ui slug` line such as `ui slug      replay-4f2c…`.
To look at a replay in the UI, restart the server pointed at that tenant, then
point it back afterwards:

```sh
DEFAULT_ORGANIZATION_SLUG=REPLACE_WITH_THE_UI_SLUG_FROM_THE_REPLAY_OUTPUT docker compose up -d server
# ... review at http://$THIS_HOST_IP:8080/ ...
docker compose up -d server                     # back to the capture tenant
```

This is safe, and here is exactly why, so you do not have to take it on faith:

- Incoming alerts are routed by the per-source token baked into the ingestion
  URL, not by this variable — the server looks up which source owns that
  token and writes the signal straight to that source's own tenant. Flipping
  `DEFAULT_ORGANIZATION_SLUG` cannot misroute or lose an incoming alert.
- `DEFAULT_ORGANIZATION_SLUG` is read exactly once, when the `server`
  container starts. It only decides which tenant the product's API and UI
  read through; it has no effect on ingestion at all.
- Recreating the `server` container to flip it takes a few seconds, during
  which any incoming POST from Grafana fails; Grafana retries, exactly as in
  section 5, so nothing is lost. You are also doing this after the capture has
  already ended, so there is nothing left to interrupt.
- While pointed at the replay tenant, the Sources page will look empty — that
  tenant has no sources of its own, only the replayed signals, incidents and
  insights. That is expected, not something you broke; go to the Overview and
  Incidents pages instead.

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
