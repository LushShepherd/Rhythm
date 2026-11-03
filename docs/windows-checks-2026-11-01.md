# Windows sleep and notification verification

Verified November 1, 2026 on Windows 11 Pro build 26300, HP ZHAN 99 Pro Tower G1i, AC power. The host advertises S0 connected standby and hibernate; S3 is unavailable. This record covers this machine and the installed Tauri 2 prototype, not general hardware certification.

## Result

**Real sleep/wake recovery and a visible native Rhythm banner passed.** A notification error-reporting defect was fixed, the installer was rebuilt, and the updated app was installed for the checks.

The test journal was isolated under `.tools/windows-checks/desktop/`. The normal app journal was not used or modified.

Automated verification passed after the fixes: **92 Rust tests and six frontend tests**, workspace formatting, and Clippy for all targets with warnings denied. The four new notification regression tests cover application identity validation, disabled settings, XML escaping/Unicode, and toast-tag limits. The release executable and NSIS installer were rebuilt and the installed release was used for the visible-banner and sleep checks.

## Notification failure and fix

The original `tauri-plugin-notification` 2.8.0 desktop implementation spawned the native send and discarded its result. Its success return did not establish that Windows had accepted the notification. The old guide overstated that evidence.

The Windows shell now calls WinRT directly. It checks `ToastNotifier.Setting`, preserves synchronous HRESULT errors, and retains `Failed` event handlers for the latest 32 attempts. Disabled app/user/policy settings return an error rather than a success message. Toast tags contain random identifiers, and diagnostics inspect only Rhythm's own history tags. Notifications contain fixed translated copy, never journal text. Normal notifications still respect Windows' Do not disturb behavior.

The installed Start menu shortcut's application identity matched `app.rhythm.quietroom`. Initial Windows Settings inspection showed Rhythm's banners enabled but global Do not disturb on. After approval to turn it off, the next inspection already showed Off; no toggle or registry write was needed by the agent. With Do not disturb off, native window capture visibly showed a toast at the lower right, with the Rhythm icon/name and the test message. This establishes visible delivery, beyond successful API submission.

The in-app success text now says Windows accepted the reminder and explains that Do not disturb can still hide its banner. The direct adapter also exposes a read-only `notification_status` command for diagnosis. No policy override, priority/alarm scenario, or global notification-setting change is performed by the app.

## Real sleep/wake evidence

The installed app started a one-minute focus period. A separate native observer ran three isolated SQLite cases: a running period, a paused period, and a 15-second period expected to expire during sleep. Both the app and observer used the production Windows clock and application persistence boundary.

An explicitly invoked diagnostic helper armed a one-shot absolute UTC wake timer, checked AC power and enabled wake-timer policy, and requested sleep. It changed no persistent power policy or scheduled task. Its temporary process privilege and native handles were restored/closed, with no cleanup error.

| Observation | Result |
| --- | --- |
| Requested wake interval | 45 seconds |
| Sleep API call/return | 10:03:37.013Z → 10:05:06.510Z |
| Helper suspend-inclusive elapsed | 89,500 ms |
| Helper awake elapsed | 8,081 ms |
| Helper excluded sleep | 81,419 ms |
| Independent observer excluded sleep | 81,424 ms |
| Observer native elapsed / UTC elapsed | 90,703 ms / 90,692 ms |
| Native callback evidence | Suspending and Resumed received |
| Wake timer | Signaled; Windows last-wake report attributed a presumed timer wake to the test process |
| Running case | Counted native elapsed including sleep |
| Paused case | Stayed paused with unchanged credited time |
| Overdue 15-second case | Completed once, capped at 15,000 ms, stayed Ready |
| Persistence | All three independently read SQLite checkpoints matched |
| Installed app | Same process survived; exactly one completed 60,000 ms focus; Ready/short-rest |
| Installed app display after wake | Completion/reflection screen; next period awaited an explicit start |

The native and UTC intervals differed by only 11 ms, and the two independent excluded-sleep measurements agreed within 5 ms. This was a real OS transition, not a simulated clock advance or thread delay. Windows Kernel-Power events included the application API request, sleep entry (42), and resume (107).

**Host limitation:** Windows returned later than the requested 45-second wake. The system log also recorded event 40 for `vpcivsp` interrupting a power transition and a later Modern Standby entry event (506). Preserve these details rather than claiming precise wake timing or deep idle residency. The application's timer recovered correctly despite the delay; Rhythm does not promise to wake the PC at a deadline. Hibernate, reboot, and other machines were not tested in this run.

Raw local evidence is retained in `.tools/windows-checks/sleep-1/` (`audit.tsv`, `result.json`, `result.txt`, and three test databases), `.tools/windows-checks/sleep-request-1.json`, and `.tools/windows-checks/os-power-events.json`. These ignored diagnostics include machine-specific event details; this document contains the relevant findings.

## Repeat the hardware check

Normal tests never suspend the computer. Use the opt-in observer and helper only during a coordinated hardware check, with no unsaved unrelated work and someone available to unlock Windows after waking.

```powershell
.\scripts\cargo.ps1 build -p rhythm-platform-windows --example observe_sleep --locked

# In one terminal; choose a NEW output directory each time:
.\target\debug\examples\observe_sleep.exe --observe --output .tools\next-sleep-check --timeout-seconds 120 --late-seconds 15

# After ready.txt appears, promptly in another terminal:
.\scripts\windows-sleep-check.ps1 -Suspend -Seconds 45
```

The helper defaults to inspect-only when `-Suspend` is omitted. It refuses sleep on battery power, disabled wake timers, unsupported wake-timer setup, or a visible power-request blocker. The current user could not inspect `/requests` without administrator elevation; this was recorded rather than bypassed. An observer run without native callbacks and at least one second excluded from the awake clock is incomplete, never a hardware pass.

## References

- [Microsoft: ToastNotifier.Setting](https://learn.microsoft.com/en-us/uwp/api/windows.ui.notifications.toastnotifier.setting) and [ToastNotification.Failed](https://learn.microsoft.com/en-us/uwp/api/windows.ui.notifications.toastnotification.failed) describe permission and asynchronous failure reporting.
- [Desktop toast application identity](https://learn.microsoft.com/en-us/windows/win32/shell/enable-desktop-toast-with-appusermodelid) describes the installed shortcut requirement.
- [SetWaitableTimer](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-setwaitabletimer) and [SetSuspendState](https://learn.microsoft.com/en-us/windows/win32/api/powrprof/nf-powrprof-setsuspendstate) describe the diagnostic request and wake timer.
- [Modern Standby states](https://learn.microsoft.com/en-us/windows-hardware/design/device-experiences/modern-standby-states) distinguishes screen-off transitions from low-power residency; this run does not certify power efficiency.
