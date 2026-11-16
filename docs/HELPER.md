# airdrop-helper: the contract

What the root helper (`crates/airdrop-helper`) promises its client. Written from the Python helper (HELPER_API 8,
`helper/airdrop-helper`, not part of this repository) before the Rust port; the Rust helper keeps all of it unless the last
column says otherwise.

## 1. Invocation and the stdin protocol

```
pkexec airdrop-helper up --iface wlan0 [--mode shared|borrow] [--channel auto|6|36|44|149] [--ble] [--fast] [-v] ...
pkexec airdrop-helper recover --iface wlan0
pkexec airdrop-helper probe --iface wlan0
airdrop-helper api                         # Rust only: prints the API level, no root needed
```

- **stdin carries no commands.** The helper lives exactly as long as the client's end of its stdin pipe: EOF
  (client exited, crashed or was killed) starts teardown. Bytes written to it are read and dropped.
- **SIGTERM** starts the same teardown. **SIGINT and SIGHUP are ignored**: the terminal's Ctrl-C belongs to the
  client, which then closes the pipe. During teardown SIGTERM is ignored too.
- Children (OWL, tools) run in their own session so the terminal's Ctrl-C cannot kill them mid-restore.
- **stdout: one JSON object per line**, always with `state`:

| `state` | Fields | Meaning |
|---|---|---|
| `notice` | `message` | Information for the user (Wi-Fi paused, stock driver loaded) |
| `warning` | `message` | Something is degraded; the session continues |
| `prefer` | `channel` | Borrow mode, since 2026-11-16 (no API bump: older clients ignore it): a peer would be better met on this channel (at least 4 of its 16 slots and more than on ours, on two checks 2 s apart), or 149 after 15 s without a peer off 149. Sent once per channel; the client decides whether to restart there |
| `inject_check` | `ok` (true/false/null), `kernel` | Do injected frames leave the driver |
| `up` | `api`, `iface` (`awdl0`), `mode`, `channel`, `ble`, `active`, `wifi_channel`, `second_channel` | AWDL is running |
| `error` | `message` | Bring-up failed, or OWL died / Wi-Fi reconnected while running. Teardown follows |
| `down` | | Teardown finished (last line of `up`) |
| `recovered` | `steps` | Reply of `recover`: names of the restore steps that ran |
| `probe` | `iface`, `phy`, `owl`, `channel`, `monitor` (+ Rust: `api`, `no_ir`, `associated`, `nm_state`, `connection`, `ht_driver`) | Read-only facts |

- Exit code 0, or 1 when bring-up failed. Rust: also 1 when a running session ended on OWL dying or Wi-Fi
  reconnecting. pkexec itself returns 126/127 when authorization is denied.
- **One session at a time (Rust, 2026-11-12):** `up` and `recover` take an `flock` on `/run/airdrop-helper.lock`
  (0600, root:root). A second `up` gets an `error` instead of "restoring" the live session and killing its OWL;
  `recover` refuses while a helper is alive. The kernel drops the lock with a killed helper, so `recover` after
  `pkill -9` works. The Python helper does not take the lock.
- Output can never abort teardown: a closed stdout/stderr is noted and ignored. Everything is also appended to
  `/run/airdrop-helper.log`; OWL's output goes to `/run/airdrop-owl.log`.

## 2. `/run/airdrop-helper.state`

JSON, rewritten atomically (`.tmp` + rename) **before** each change it describes; removed after teardown. A new
`up` that finds one restores it first. `recover` replays it, and also checks the things marked * without it.

| Key | Type | Undo |
|---|---|---|
| `iface` | string | the Wi-Fi interface everything refers to |
| `phase` | `Idle` / `Borrowed` / `Running` / `TearingDown` | Rust only; informational, the keys below say what to undo |
| `owl_pid` | int or null | SIGTERM, 5 s, SIGKILL (Rust: only if the pid still is OWL); then every process whose exe is an OWL path * |
| `ble` | bool | advert registered. Rust: nothing to undo (it dies with the D-Bus connection) |
| `ble_pid` | int | Python only: the `bluetoothctl` holding the advert; SIGTERM (Rust: only if `/proc/<pid>/comm` still is `bluetoothctl`) |
| `vifs` | list | informational; `awdlact` and `awdlmon` are deleted whenever they exist * |
| `go`, `go_mac` | bool, string | informational; `awdlgo` is stopped and deleted whenever it exists * |
| `nm_go_conf` | bool | remove `/run/NetworkManager/conf.d/90-airdrop-go.conf` *, reload NM conf |
| `nm_borrow_conf` | bool | remove `/run/NetworkManager/conf.d/90-airdrop-borrow.conf` *, reload NM conf |
| `pm` | {path: old value} | write the mt76 `runtime-pm` / `deep-sleep` values back (`recover` assumes 1 *) |
| `debugfs_mounted` | bool | unmount `/sys/kernel/debug` |
| `nm_disconnected` | `{uuid, name, autoconnect_only, ap}` or null | unless `autoconnect_only`: activate `uuid` (first round: on access point `ap` if recorded, API 9), else any connection (3 rounds); then autoconnect on (last since API 10). `recover` adds it when NM reports the device disconnected * |
| `wlan_down`, `nm_released` | bool | legacy (exclusive mode): type managed + link up; NM managed = yes. Still undone, never set by Rust |

Restore order: OWL, BLE, monitor vifs, GO, NM rules, power saving, debugfs, legacy, Wi-Fi reconnect. Every step runs
whatever the others did.

## 3. External commands of the Python helper, and what replaced them

| Command | Purpose | Rust |
|---|---|---|
| `iw dev X info` / `link` | station channel, associated? | nl80211 GET_INTERFACE / GET_STATION dump |
| `iw phy P info` / `channels` | monitor support, 2-channel combos, No-IR | nl80211 GET_WIPHY (split dump); combos check dropped with `--go-channel` |
| `iw phy P interface add N type monitor [flags active]`, `type __p2pgo`; `iw dev N del` | vifs | NEW_INTERFACE / DEL_INTERFACE |
| `iw dev awdlmon set channel` | exclusive mode only | dropped |
| `iw dev awdlgo ap start DIRECT-ad <freq> NOHT\|HT40+ 100 1 head <hex>`; `ap stop` | hidden GO | START_AP / STOP_AP |
| `iw dev X offchannel <freq> 5000` | `--borrow-radio roc` | dropped |
| `iw event`, `iw dev X scan abort`, `iw … scan sched_stop` | scan guard | "scan" multicast group; ABORT_SCAN / STOP_SCHED_SCAN |
| `iw dev X set type managed` | legacy restore | SET_INTERFACE |
| `ip link set N address M` / `up` / `down` | vif MAC and state | SIOCSIFHWADDR / SIOCSIFFLAGS (never down) |
| `ip -4 route show default`, `ping` | second-channel watchdog, `gotest` | dropped |
| `ip -6 route replace fe80::/64 dev awdl0 … congctl bbr`, `modprobe tcp_bbr` | `--bbr` (opt-in) | same commands |
| `nmcli -t -f GENERAL.STATE device show` | device state | D-Bus `Device.State` |
| `nmcli -t -f UUID,NAME,DEVICE connection show --active` | active connection | `Device.ActiveConnection` → `Uuid`, `Id` |
| `nmcli device disconnect` | pause Wi-Fi | `Device.Disconnect()` |
| `nmcli device set X autoconnect yes\|no` / `managed yes\|no` | | `Device.Autoconnect` / `Device.Managed` |
| `nmcli connection up uuid U ifname X`, `nmcli device connect X` | reconnect | `ActivateConnection(conn or "/", device, "/")`, wait for state 100 |
| `nmcli general reload conf` | after writing/removing a drop-in | `Reload(1)` |
| `bluetoothctl` (power on, manufacturer 0x004c …, interval 100 100, advertise broadcast) | BLE advert | BlueZ `LEAdvertisingManager1.RegisterAdvertisement` with our `LEAdvertisement1` object |
| `btmgmt power on`, `add-adv -g -d … 7`, `rm-adv 7` | fallback advert | dropped (no fallback) |
| `resolvectl mdns awdl0 no`, `llmnr awdl0 no` | keep systemd-resolved off awdl0 | same commands |
| `tcpdump -i awdlmon -c 20 -e` | which frequency is heard | AF_PACKET + radiotap |
| `modinfo -F srcversion mt7921e` | is the stock driver loaded | same command |
| `mount -t debugfs`, `umount` | reach the mt76 knobs | mount(2) / umount(2) |
| OWL binary | AWDL | same; flags below |

Files read: `/sys/class/net/*`, `/sys/module/mt7921e/srcversion`, `/run/airdrop-mt7921-ht`,
`/sys/kernel/debug/ieee80211/<phy>/mt76/{runtime-pm,deep-sleep}`, the OWL binary (for its usage strings).

## 4. OWL command line (borrow mode on 5 GHz, all defaults)

`owl -i awdlact -c 149 -N -P 10000 -I -S -H -V -R -C 149 -r auto-ht|auto -A -p 16 [-G awdlgo]`

`-G awdlgo` with `--tx-agg` (what the client passes for `send` since API 11) when the loaded mt7921e has the
`awdl_ba` parameter.

Each optional flag is added only when the installed OWL has its usage string. `-F` `--static-seq`, `-M` `--election-metric`,
`-v`; `-V` only while the GO runs at 80 MHz (API 10 default on 149-161; `--ht40` drops it and makes the GO 40 MHz);
`--narrow` drops `-H` and makes the GO 20 MHz; `--no-ampdu` drops `-A`; `--presence 0` drops `-p`; `--tx-rate`
replaces the automatic rate (12 = OWL's own default, no `-r`). Shared mode: `-i awdlact -c <wifi channel> -N -P 10000`
plus the rate on 5 GHz.

**`--tx-agg` (aggregated sending, API 11; the client passes it for `send` in borrow mode).** The helper
- starts the GO with a beacon tail (`radio::beacon_tail`: HT/VHT Capabilities and Operation),
- writes `1` to `/sys/module/mt7921e/parameters/awdl_ba` (old value kept in the state file's `pm` map and restored
  like the power-saving knobs; `recover` writes `0` whenever the parameter exists),
- adds `-G awdlgo` to OWL,
- at teardown logs `iw dev awdlgo station dump` and each station's `ht_capa`, `vht_capa`, `agg_status` from debugfs.

Without the driver build that has the parameter it logs one line and changes nothing; outside borrow mode it warns.
Leftovers of the experiment, to be removed: `--tx-agg-bw 1|2` (driver `awdl_bw`, no effect), `--ba-probe` (OWL `-B`).

## 5. HELPER_API history

The level is reported in `up` (`api`); the client warns when it is lower than it expects.

| Level | Date | What the client started to rely on |
|---|---|---|
| 1–2 | 2026-11-11 | Version reporting introduced (EXPERIMENTS "Stability changes"). What separated 1 from 2 was not recorded |
| 3 | 2026-11-11 | Borrow defaults: `-r 54` on 5 GHz and Block Ack `-A` (`--tx-rate`, `--no-ampdu` escapes) |
| 4 | 2026-11-11 | Presence window `-p 16` in borrow mode (`--presence`) |
| 5 | 2026-11-11 | `--fast`: shorter waits and GO start retries, for `airdrop listen` |
| 6 | 2026-11-11 | Helper children in their own session (immune to the terminal's Ctrl-C) |
| 7 | 2026-11-12 | Rate `auto` / `auto-ht` on 5 GHz, HT driver detected through `/run/airdrop-mt7921-ht`; scan guard default in borrow mode |
| 8 | 2026-11-12 | HT driver also recognised by srcversion (`HT_DRIVER_SRCVERSIONS`) |
| 9 | 2026-11-14 | Short bring-up by default: in borrow mode the scan guard starts right after Wi-Fi is paused, the settle wait is 500 ms (as `--fast` was), the GO start retries, no frame check (`--fast` is accepted, no longer needed; hidden `--careful` restores API 8's sequence). BLE advert before `resolvectl`. Reconnect tries the access point Wi-Fi was on (`nm_disconnected.ap`). `timing:` lines in the log |
| 10 | 2026-11-14 | Borrow on 149-161: the GO at **80 MHz** and OWL `-V` (VHT caps) by default (E-VHT80: 14.8 → 21.0 MB/s); 40 MHz fallback if 80 MHz is refused; `-V` only when nl80211 reports 80 MHz. Hidden `--ht40` = API 9; `--vht80` adds 36-48. Reconnect activates the paused connection before device auto-connect goes back on; 50 ms state polling |
| 11 | 2026-11-16 | `--tx-agg` (the client passes it for `send` in borrow mode): OWL `-G awdlgo`, driver `awdl_ba` on for the session, full GO beacon; 5.4 → 30.6 MB/s on 417 MB. Used only with the mt7921e build that has the parameter and an OWL with `-G`; otherwise single frames as in API 10 |
| 11 (same number) | 2026-11-16 | Borrow mode also accepts channel **36** (an iPhone joined to a network on 36 mostly does not answer on 149; EXPERIMENTS C-2); `prefer` message |
