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
- Port 8080 is what Grafana and your workstation talk to. Two things protect it,
  and both are set in section 2: the product API requires a token
  (`API_TOKEN`), and ingestion requires the per-source token baked into each
  ingestion URL. Do not rely on `ufw` to restrict the port: Docker publishes
  ports by rewriting packets before `ufw` sees them, so a `ufw allow from` rule
  looks like a restriction and is not one. If you also want a network
  restriction, use Docker's own hook and then verify it from a third machine:

  ```sh
  sudo iptables -I DOCKER-USER 1 -p tcp --dport 8080 -j DROP
  sudo iptables -I DOCKER-USER 1 -p tcp --dport 8080 -s REPLACE_WITH_GRAFANA_IP -j RETURN
  sudo iptables -I DOCKER-USER 1 -p tcp --dport 8080 -s REPLACE_WITH_YOUR_WORKSTATION_IP -j RETURN
  ```

  From any other machine, `curl -m 3 http://REPLACE_WITH_THIS_HOST_IP:8080/health`
  must time out. These rules do not survive a reboot on their own; on Debian or
  Ubuntu, `sudo apt install iptables-persistent` and `sudo netfilter-persistent save`
  keep them. Skip this block entirely if you are unsure; the token is the guarantee.

## 2. Start it

Check whether this host already has a deployment before touching anything:

```sh
docker compose ps
```

- If that command fails (no such file, no checkout yet) or its output is just
  the header row with nothing under it, this is a fresh start — continue with
  "First time on this host" below.
- If it lists containers — look in the leftmost NAME column for
  `ops-intelligence-db` and `ops-intelligence-server` (the SERVICE column
  next to it shows the shorter `postgres` and `server`) — skip straight to
  "Already running" below. Do not clone on top of an existing checkout or run
  `--build` without reading that section first — a capture may be in progress.

### First time on this host

```sh
git clone REPLACE_WITH_REPO_URL ops-intelligence && cd ops-intelligence   # the author gives you this URL at handoff
export THIS_HOST_IP=REPLACE_WITH_THIS_HOST_IP   # the address Grafana and your workstation use to reach this host — the same one allowed through the firewall in section 1; find it with: hostname -I
export API_BASE_URL=http://$THIS_HOST_IP:8080   # baked into the ingestion URL the UI (and the curl equivalent in section 3) will show you later — set it now, before either exists
export POSTGRES_PASSWORD=REPLACE_WITH_A_PASSWORD   # anything; it is only reachable on this host
export API_TOKEN=$(openssl rand -hex 24)   # the product API's password; the UI asks for it once — print it with: echo $API_TOKEN
export BIND_ADDR=0.0.0.0                   # publish port 8080 to the network; without this the stack is reachable only from this host
docker compose up -d --build
until curl -sf localhost:8080/health; do sleep 2; done
```

The loop waits for the server to finish starting, prints the health line once,
and stops. If it runs for more than a minute, look at `docker compose logs
server`.

Expected: `{"status":"ok"}`. From your workstation, open `http://` followed by
your real host address and `:8080/` (not the literal text `$THIS_HOST_IP`).
You should see the Overview page with zeros. The first page will ask for the
API token; paste the value of `echo $API_TOKEN`. It is kept in that browser only.

Write down the value you used for `THIS_HOST_IP` and the password you picked
— nothing persists them for you. You need them again in any new shell before
running `docker compose run` or `docker compose up` (they create containers
and substitute these values in at that moment); `docker compose exec` and
`docker compose ps` do not need them, since those act on containers that
already exist. If you ever lose the values — including if you are not the
person who started the capture — "Already running" below shows how to recover
them from the running containers instead of guessing.

### Already running

The stack came up before — a reboot, a shift change, or you are back for
section 5 or 6, and are not necessarily the person who ran "First time on
this host". Recover the values from the running containers even if you think
you still have them. The commands are cheap, they cannot be wrong, and the
last line of the export block catches a stack that was started without an
API token, which a remembered value would skip straight past:

```sh
docker compose exec server printenv API_BASE_URL
docker compose exec postgres printenv POSTGRES_PASSWORD
docker compose exec server printenv API_TOKEN
docker compose port server 8080          # prints 0.0.0.0:8080 when published to the network
```

`API_BASE_URL` already contains the host address that was used when the stack
was started, so its output is the authoritative answer to "what was
`THIS_HOST_IP`" — read the address back out of it. Do not guess `localhost`:
a source created with the wrong address still returns a perfectly normal
`201` with an `ingest_url` in it, and the only symptom is that Grafana,
running on a different host, can never reach that URL. Then:

```sh
export API_BASE_URL=$(docker compose exec -T server printenv API_BASE_URL)
export THIS_HOST_IP=REPLACE_WITH_THE_ADDRESS_FROM_API_BASE_URL_ABOVE
export POSTGRES_PASSWORD=$(docker compose exec -T postgres printenv POSTGRES_PASSWORD)
export API_TOKEN=$(docker compose exec -T server printenv API_TOKEN)
export BIND_ADDR=0.0.0.0
[ -n "$API_TOKEN" ] || { export API_TOKEN=$(openssl rand -hex 24); echo "This stack was started WITHOUT an API token. One was just generated; run: docker compose up -d server   and then paste it into the UI: echo \$API_TOKEN"; }
```

The `-T` matters: without it `docker compose exec` allocates a terminal and
appends a carriage return, which `$(...)` does not strip, so the value would
end up with an invisible carriage return on the end.

The last line covers a stack that was started without a token: `printenv`
then prints nothing and exits successfully, so without the check the empty
value would be carried into the next `up` and the product API would come back
up unprotected, silently. The check generates a token instead and tells you to
recreate the server so it takes effect.

If the value you just read back contains `localhost` or `127.0.0.1`, stop:
that is the compose default when nobody exported `API_BASE_URL` before the
stack was created, not this host's real address, and no off-host Grafana can
ever reach it. A source created against it still returns a perfectly normal
`201` with an `ingest_url` in it — nothing about the response tells you it is
wrong. Correct it before creating any source: export the real address instead
(the one from section 1, not `localhost`), then recreate the `server`
container so it picks it up, `docker compose up -d server`, and read it back
again to confirm.

Not every command below actually needs these re-exported, and it matters
which:

- `docker compose exec ...` (used again in section 5, and for the backup in
  section 6) attaches to a container that is already running. Nothing is
  substituted when you run it, so it works whether or not anything is
  exported in this shell — this is exactly why the recovery commands above
  work even with no `THIS_HOST_IP`, `API_BASE_URL` or `POSTGRES_PASSWORD` set.
- `docker compose run --rm worker ...` and `docker compose up ...` **create**
  a container, and compose substitutes `${POSTGRES_PASSWORD:-ops_local_dev}`
  into it at that moment, from whatever is or is not exported in your current
  shell. If the database was set up with a custom password and you have not
  re-exported it before one of these, the new container silently gets the
  default instead and fails to authenticate — re-export before any `run` or
  `up`, not before every command.

Section 3 asked you to write down the source name — if that got lost too,
recover it from the API instead of guessing from the Sources page:

```sh
curl -s -H "authorization: Bearer $API_TOKEN" http://localhost:8080/api/v1/sources
```

Every source returned has `name`, `source_type` and `last_seen_at`. The
capture source is the one with `source_type` `grafana` and a recent
`last_seen_at`; a source that was created but never wired into Grafana shows
`last_seen_at` of `null`. If more than one `grafana` source has recent
traffic, stop and ask rather than guess — freezing the wrong one in section 6
does not fail loudly, it just builds a small, wrong dataset (see section 6).

Then:

- To confirm it is healthy without changing anything: `docker compose ps` —
  look at the NAME column; you should see `ops-intelligence-db` and
  `ops-intelligence-server` (the SERVICE column shows the shorter `postgres`
  and `server`), both `healthy`. That is normally all you need here.
- Only if the author sent you a new checkout to pick up: `docker compose up -d
  --build`. This recreates the `server` container, so ingestion is
  unavailable for the few seconds that takes; Grafana retries a failed
  delivery several times before giving up (the same retry behaviour section 7
  relies on when you pause a source), so nothing is lost. It does not touch
  anything stored in the `ops-pgdata` volume.
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
  -H "authorization: Bearer $API_TOKEN" \
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
   `Max alerts` times (bytes per alert plus about 600 bytes for Grafana's
   rendered digest of that alert) under 4 MiB with margin; a 4 KB alert
   therefore counts as about 4.6 KB.
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

Then tell the author which values your alerts' `severity` label takes (for
example `critical`, `warning`, `page`, `P1`). The engine refuses to guess what a
severity word means, so an alert whose value is not in its table is stored but
produces no event until the table is updated. The day-one check in section 5
shows exactly which values, if any, need adding.

## 5. During the capture (once a day, two minutes)

**Day one, once real alerts have arrived** (the Sources page shows a recent
"Last seen"): run one smoke replay, so a capture the engine cannot read is found
today instead of on day five. Use the source name from section 3.

```sh
docker compose run --rm worker pilot dataset create day-$(date +%F) REPLACE_WITH_THE_SOURCE_NAME_FROM_SECTION_3
docker compose run --rm worker pilot replay day-$(date +%F) smoke
```

The dataset name carries the date, so the check can be repeated on any day
without a name clash.

Expected: an `events` line ending in `(0 failed)`. If the failed count is not
zero, list the reasons and send them to the author the same day:

```sh
docker compose exec -T postgres psql -U ops -d ops_intelligence -c \
  "SELECT processing_error, COUNT(*) FROM raw_signals WHERE processing_status = 'failed' GROUP BY 1 ORDER BY 2 DESC"
```

The capture continues regardless; nothing here touches it. The author fixes
the mapping on their side before the final replay in section 6.

- Overview page, first line: the signal count should be higher than
  yesterday — write today's number down before you close the tab; it is the
  only record of "yesterday" the next person to check (maybe a different
  shift) will have. Events and incidents stay at zero for the whole capture on
  a host used only for this pilot; that is by design, not a fault, because
  nothing is analysed until the replay in section 6. A non-zero count here
  means one of two things, not a broken rule: either the host already had
  data on it before the capture started, or someone ran `process` or
  `correlate` against the live tenant despite the warning below.
- Sources page: "Last seen" is recent. If it is more than a few hours old on a
  normal day, check `docker compose ps` and the firewall.
- `docker compose logs server | grep -i -E "truncated|rejected|exceeds"`: any line
  here means Grafana sent something the engine could not take whole. Section 9
  says what each one means; note it for the review session. No output — grep
  exits non-zero — is the healthy result; it does not mean the command failed.

Do not run `process` or `correlate` against the live tenant during the capture.
The capture tenant stays raw; the analysis happens by replay (section 6). This is
what makes the week re-runnable.

## 6. End of capture

Back it up first. The captured week is the asset; the code is replaceable.

```sh
docker compose exec -T postgres pg_dump -U ops -Fc ops_intelligence > ~/capture-$(date +%F).dump
docker compose exec -T postgres pg_restore --list < ~/capture-$(date +%F).dump | head -5
```

The `-T` is not optional here: without it, compose allocates a terminal, and a
terminal rewrites line endings inside what is a binary file. The second command
reads the dump back through PostgreSQL's own restore tool and prints its table of
contents; if it prints an error instead of a few `;` header lines, the backup is
not usable, so do not proceed until it does. The file lands in your home
directory, not the `ops-intelligence` checkout you `cd`'d into in section 2 —
keep it outside the checkout so a copy-paste never drops a database dump into the
source tree. If you are already somewhere else, use that path instead; the point
is "not inside the checkout", not the specific directory.

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
not created` rather than silently building an empty dataset. That automatic
refusal only catches a *zero* match, though: a wrong-but-plausible source
name — a leftover test source that did receive a handful of pings, say —
freezes successfully with no error and a small, wrong `SIGNALS` count.
Reading that number is therefore the real check here, not a formality; the
automatic refusal is a backstop, not a substitute for looking. Either way,
sort that out here; do not continue to the replay below on a dataset you have
not confirmed.

Replay it twice and confirm the two runs agree:

```sh
docker compose run --rm worker pilot replay week-01 run-a
docker compose run --rm worker pilot replay week-01 run-b
docker compose run --rm worker pilot compare run-a run-b
```

Expected: both replays print the same counts, and compare reports no deltas.

Each replay also prints a `failed` count. A non-zero figure means some captured
payloads could not be understood; they are kept, and the review session looks at
them. The "failed signals" tile on the Overview shows the same number once the UI
is pointed at the replay tenant, below.

Each replay also prints a `ui slug` line such as `ui slug      replay-4f2c…`.
To look at a replay in the UI, restart the server pointed at that tenant, then
point it back afterwards:

This recreates the `server` container, so the values from section 2 must be
exported in this shell first: `POSTGRES_PASSWORD`, or the new container cannot
reach the database; `API_BASE_URL`, or any source created afterwards prints an
ingestion URL Grafana cannot reach; `API_TOKEN`, or the bearer check disables
itself and the product API that mints ingestion tokens and resolves incidents
is wide open again for the rest of the capture; and `BIND_ADDR`, or the port
falls back to loopback and Grafana can no longer deliver anything at all,
silently, until someone notices "Last seen" going stale. If you are not sure
they are set, run the recovery commands under "Already running" in section 2 now.

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
  which any incoming POST from Grafana fails; Grafana retries a failed
  delivery several times before giving up (the same retry behaviour section 7
  relies on when you pause a source), so nothing is lost. You are also doing
  this after the capture has already ended, so there is nothing left to
  interrupt.
- While pointed at the replay tenant, the Sources page shows a copy of the
  capture source under the same name, marked `offline import` because the copy
  has no ingestion token. It is a replay artefact. Disabling it does nothing to
  real ingestion, and the real source cannot be seen or paused from here.
  Section 7's pause step assumes the server is pointed at the capture tenant:
  run `docker compose up -d server` first if you are still viewing a replay.

## 7. Stop or roll back

- Grafana: delete the nested policy (section 4 step 3). Notifications to the
  engine stop instantly; nothing else changes.
- Host: `docker compose down`. Data stays in the `ops-pgdata` volume. To remove
  it entirely: `docker compose down -v`, after the backup in section 6.
- To pause without removing: with the server pointed at the capture tenant
  (run `docker compose up -d server` first if you were viewing a replay),
  disable the source on the Sources page, the one marked `webhook token set`.
  Grafana will get a 400 and give up after its retries.

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
  refused and logged as `grafana batch rejected`. At real Grafana shapes this
  is not expected. If the line appears, tell the author: the notification was
  shaped unlike anything the engine expects.
- A generic-webhook body the engine cannot read is refused and logged as
  `webhook payload rejected`. Same instruction.
- Which alert labels mean environment, service and resource are fixed guesses
  (`environment`/`env`, `service`/`job`/`app`, `instance`/`host`/`resource`/`pod`).
  Real data will correct them; that is expected.
- Severity comes from a label named `severity`. Alerts without it are treated as
  warning while firing.
- Everything the engine computes is deterministic and re-runnable. Nothing you do
  in the UI changes the captured signals.
