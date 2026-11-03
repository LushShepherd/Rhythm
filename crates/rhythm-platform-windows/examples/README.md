# Opt-in hardware sleep observer

This executable observes a separately coordinated Windows sleep/wake cycle. It
does not request sleep, arm a wake timer, change power policies, or use the user's
Rhythm database. Supply a new output directory whose parent already exists:

```powershell
.\scripts\cargo.ps1 run -p rhythm-platform-windows --example observe_sleep '--' --observe --output .\qa\hardware-sleep-run --timeout-seconds 180 --late-seconds 30
```

Wait for `ready.txt` or the JSON `ready` output. Begin sleep before the 30-second late
deadline and wake after that deadline, within the observer timeout. The observer
records each native event at worker receipt time (events can be delayed), the
actual Windows inclusive timer and unbiased awake-time counter, and three isolated
SQLite databases. It verifies that a long running period, a paused period, and
a short period whose deadline passes during sleep are each ended as interrupted
at their last verified elapsed checkpoint. All remain Ready for explicit Focus;
none completes during sleep or starts a catch-up cycle. Duplicate interruption
and polling must not create another session. Checkpoints are read through
independent database connections.

`result.txt` and `result.json` report a pass only if both native suspend/resume callbacks were
received, the clocks show at least one second excluded from awake time, and all
timer/database checks passed. A thread delay cannot pass. `audit.tsv` and the
databases are retained even for an inconclusive result. Standard output contains
one JSON object per line for readiness and the final result. A smoke run that does not
sleep is expected to exit unsuccessfully and report missing hardware evidence.

For Modern Standby (S0 low power idle), preserve Windows Kernel-Power transition
events or a SleepStudy report as independent evidence too. The desktop activity
moderator can pause an app, so callback timestamps identify when the worker
received an event, not the exact instant the hardware changed power state. If
native elapsed counters do not establish a sleep interval, do not call that a
passed hardware test based only on a delayed worker.

Before any separately authorized automated sleep, inspect `powercfg /a` and the
current `RTCWAKE` power setting. A transient wake timer must use an absolute UTC
FILETIME and `SetWaitableTimer` with `fResume=TRUE`, check both failure and
`ERROR_NOT_SUPPORTED`, and be canceled and closed afterward. Relative waitable
timers stop counting in low-power states on Windows 8 and later. Wake timers may
be disabled on battery and a timer API success does not prove a future hardware
wake; do not change system policy merely to make a test pass. This observer does
not implement or invoke the sleep/wake-control procedure.

Primary references:

- [QueryUnbiasedInterruptTime](https://learn.microsoft.com/en-us/windows/win32/api/realtimeapiset/nf-realtimeapiset-queryunbiasedinterrupttime)
- [SetWaitableTimer](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-setwaitabletimer)
- [SetSuspendState](https://learn.microsoft.com/en-us/windows/win32/api/powrprof/nf-powrprof-setsuspendstate)
- [Modern Standby integration](https://learn.microsoft.com/en-us/windows-hardware/design/device-experiences/integrating-apps-with-modern-standby)
