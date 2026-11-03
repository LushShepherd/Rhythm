<#
Opt-in hardware check. The default only reads capabilities and compiles the helper.
Run -Suspend -Seconds 30 only after the app and hardware observer are ready.
No persistent settings, scheduled tasks, registry values, or power policies change.

API contracts:
https://learn.microsoft.com/windows/win32/api/synchapi/nf-synchapi-setwaitabletimer
https://learn.microsoft.com/windows/win32/api/powrprof/nf-powrprof-setsuspendstate
https://learn.microsoft.com/windows/win32/api/securitybaseapi/nf-securitybaseapi-adjusttokenprivileges
#>
[CmdletBinding()]
param(
    [switch]$Suspend,
    [ValidateRange(5, 60)]
    [int]$Seconds = 30
)

$ErrorActionPreference = 'Stop'
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'This optional hardware check requires Windows.'
}

if (-not ('RhythmHardwareSleep.NativeCheck' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace RhythmHardwareSleep {
    public sealed class PowerSnapshot {
        public byte ACLineStatus;
        public uint ACWakeTimerPolicy;
        public string ActiveScheme;
        public bool CanAttemptTimerWake;
    }

    public sealed class SleepResult {
        public string StartedUtc;
        public string WakeDueUtc;
        public string SuspendCallReturnedUtc;
        public string FinishedUtc;
        public bool NativeRequestAccepted;
        public bool WakeTimerSignaled;
        public ulong InclusiveElapsedMs;
        public ulong AwakeElapsedMs;
        public ulong ExcludedSleepMs;
        public string Error;
        public int NativeError;
        public string CleanupError;
    }

    public static class NativeCheck {
        [StructLayout(LayoutKind.Sequential)]
        private struct SYSTEM_POWER_STATUS {
            public byte ACLineStatus, BatteryFlag, BatteryLifePercent, SystemStatusFlag;
            public uint BatteryLifeTime, BatteryFullLifeTime;
        }
        [StructLayout(LayoutKind.Sequential)]
        private struct LUID { public uint LowPart; public int HighPart; }
        [StructLayout(LayoutKind.Sequential)]
        private struct TOKEN_PRIVILEGES {
            public uint PrivilegeCount;
            public LUID Luid;
            public uint Attributes;
        }

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool GetSystemPowerStatus(out SYSTEM_POWER_STATUS status);
        [DllImport("powrprof.dll")]
        private static extern uint PowerGetActiveScheme(IntPtr root, out IntPtr scheme);
        [DllImport("powrprof.dll")]
        private static extern uint PowerReadACValueIndex(IntPtr root, ref Guid scheme, ref Guid subgroup, ref Guid setting, out uint value);
        [DllImport("kernel32.dll")]
        private static extern IntPtr LocalFree(IntPtr memory);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern SafeWaitHandle CreateWaitableTimer(IntPtr attributes, [MarshalAs(UnmanagedType.Bool)] bool manualReset, string name);
        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool SetWaitableTimer(SafeWaitHandle timer, ref long dueTime, int period, IntPtr callback, IntPtr context, [MarshalAs(UnmanagedType.Bool)] bool resume);
        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool CancelWaitableTimer(SafeWaitHandle timer);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern uint WaitForSingleObject(SafeWaitHandle handle, uint milliseconds);
        [DllImport("kernel32.dll")]
        private static extern void SetLastError(uint error);
        [DllImport("kernel32.dll")]
        private static extern IntPtr GetCurrentProcess();
        [DllImport("kernel32.dll")]
        private static extern ulong GetTickCount64();
        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool QueryUnbiasedInterruptTime(out ulong count);
        [DllImport("advapi32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool OpenProcessToken(IntPtr process, uint access, out SafeAccessTokenHandle token);
        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool LookupPrivilegeValue(string system, string name, out LUID luid);
        [DllImport("advapi32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool AdjustTokenPrivileges(SafeAccessTokenHandle token, [MarshalAs(UnmanagedType.Bool)] bool disableAll, ref TOKEN_PRIVILEGES state, uint size, out TOKEN_PRIVILEGES previous, out uint returned);
        [DllImport("advapi32.dll", EntryPoint = "AdjustTokenPrivileges", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool RestoreTokenPrivileges(SafeAccessTokenHandle token, [MarshalAs(UnmanagedType.Bool)] bool disableAll, ref TOKEN_PRIVILEGES previous, uint size, IntPtr ignoredState, IntPtr ignoredLength);
        [DllImport("powrprof.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.U1)]
        private static extern bool SetSuspendState([MarshalAs(UnmanagedType.U1)] bool hibernate, [MarshalAs(UnmanagedType.U1)] bool force, [MarshalAs(UnmanagedType.U1)] bool disableWakeEvents);

        private static void Check(bool success) {
            if (!success) throw new Win32Exception(Marshal.GetLastWin32Error());
        }

        public static PowerSnapshot Inspect() {
            SYSTEM_POWER_STATUS status;
            Check(GetSystemPowerStatus(out status));
            IntPtr allocatedScheme;
            uint result = PowerGetActiveScheme(IntPtr.Zero, out allocatedScheme);
            if (result != 0) throw new Win32Exception((int)result);
            try {
                Guid scheme = (Guid)Marshal.PtrToStructure(allocatedScheme, typeof(Guid));
                Guid subgroup = new Guid("238c9fa8-0aad-41ed-83f4-97be242c8f20");
                Guid setting = new Guid("bd3b718a-0680-4d9d-8ab2-e1d2b4ac806d");
                uint wakePolicy;
                result = PowerReadACValueIndex(IntPtr.Zero, ref scheme, ref subgroup, ref setting, out wakePolicy);
                if (result != 0) throw new Win32Exception((int)result);
                return new PowerSnapshot {
                    ACLineStatus = status.ACLineStatus,
                    ACWakeTimerPolicy = wakePolicy,
                    ActiveScheme = scheme.ToString(),
                    CanAttemptTimerWake = status.ACLineStatus == 1 && wakePolicy == 1
                };
            } finally { LocalFree(allocatedScheme); }
        }

        public static SleepResult SuspendFor(int seconds) {
            if (seconds < 5 || seconds > 60) throw new ArgumentOutOfRangeException("seconds");
            SleepResult report = new SleepResult();
            report.StartedUtc = DateTime.UtcNow.ToString("o");
            SafeWaitHandle timer = null;
            SafeAccessTokenHandle token = null;
            TOKEN_PRIVILEGES previous = new TOKEN_PRIVILEGES();
            bool restorePrivilege = false;
            bool timerArmed = false;
            ulong inclusiveStart = 0, awakeStart = 0;
            bool sampled = false;
            try {
                if (!Inspect().CanAttemptTimerWake)
                    throw new InvalidOperationException("Refusing sleep: AC power and fully enabled AC wake timers are required.");
                LUID shutdown;
                Check(LookupPrivilegeValue(null, "SeShutdownPrivilege", out shutdown));
                Check(OpenProcessToken(GetCurrentProcess(), 0x0020 | 0x0008, out token));
                TOKEN_PRIVILEGES desired = new TOKEN_PRIVILEGES { PrivilegeCount = 1, Luid = shutdown, Attributes = 2 };
                uint returned;
                SetLastError(0);
                bool adjusted = AdjustTokenPrivileges(token, false, ref desired, (uint)Marshal.SizeOf(typeof(TOKEN_PRIVILEGES)), out previous, out returned);
                int adjustError = Marshal.GetLastWin32Error();
                restorePrivilege = adjusted;
                if (!adjusted || adjustError != 0) throw new Win32Exception(adjustError);

                timer = CreateWaitableTimer(IntPtr.Zero, true, null);
                if (timer == null || timer.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
                DateTime due = DateTime.UtcNow.AddSeconds(seconds);
                long fileTime = due.ToFileTimeUtc(); // Positive absolute UTC, never a relative timeout.
                report.WakeDueUtc = due.ToString("o");
                SetLastError(0);
                bool armed = SetWaitableTimer(timer, ref fileTime, 0, IntPtr.Zero, IntPtr.Zero, true);
                int timerError = Marshal.GetLastWin32Error();
                timerArmed = armed;
                if (!armed || timerError == 50) throw new Win32Exception(timerError);

                // Recheck immediately before sleep in case the AC source or plan changed.
                if (!Inspect().CanAttemptTimerWake)
                    throw new InvalidOperationException("Power conditions changed before sleep; the test was canceled.");
                inclusiveStart = GetTickCount64();
                Check(QueryUnbiasedInterruptTime(out awakeStart));
                sampled = true;
                // All flags false: sleep rather than hibernate, no force request,
                // and retain every wake event. Windows documents force as ignored.
                Check(SetSuspendState(false, false, false));
                report.NativeRequestAccepted = true;
                report.SuspendCallReturnedUtc = DateTime.UtcNow.ToString("o");
                // Keep the one-shot timer alive if a platform returns early from
                // the request. No repeating sleep requests or blocker overrides.
                uint wait = WaitForSingleObject(timer, (uint)((seconds + 10) * 1000));
                report.WakeTimerSignaled = wait == 0;
                if (wait == 0xFFFFFFFF) throw new Win32Exception(Marshal.GetLastWin32Error());
                if (wait != 0) throw new InvalidOperationException("The wake timer did not signal before the diagnostic wait expired.");
            } catch (Exception error) {
                report.Error = error.Message;
                Win32Exception nativeError = error as Win32Exception;
                if (nativeError != null) report.NativeError = nativeError.NativeErrorCode;
            } finally {
                if (sampled) {
                    report.InclusiveElapsedMs = GetTickCount64() - inclusiveStart;
                    ulong awakeEnd;
                    if (QueryUnbiasedInterruptTime(out awakeEnd)) {
                        report.AwakeElapsedMs = (awakeEnd - awakeStart) / 10000;
                        report.ExcludedSleepMs = report.InclusiveElapsedMs > report.AwakeElapsedMs ? report.InclusiveElapsedMs - report.AwakeElapsedMs : 0;
                    }
                }
                if (timer != null) {
                    if (timerArmed && !CancelWaitableTimer(timer)) report.CleanupError = "Wake timer cancellation: " + Marshal.GetLastWin32Error();
                    timer.Dispose();
                }
                if (token != null) {
                    if (restorePrivilege) {
                        SetLastError(0);
                        bool restored = RestoreTokenPrivileges(token, false, ref previous, 0, IntPtr.Zero, IntPtr.Zero);
                        int restoreError = Marshal.GetLastWin32Error();
                        if (!restored || restoreError != 0) report.CleanupError = (report.CleanupError ?? "") + " Privilege restoration: " + restoreError;
                    }
                    token.Dispose();
                }
                report.FinishedUtc = DateTime.UtcNow.ToString("o");
            }
            return report;
        }
    }
}
'@
}

$rhythmPowerStates = (& powercfg /a 2>&1 | Out-String).Trim()
$rhythmStatesExit = $LASTEXITCODE
$rhythmPowerRequests = (& powercfg /requests 2>&1 | Out-String).Trim()
$rhythmRequestsExit = $LASTEXITCODE
$rhythmPower = [RhythmHardwareSleep.NativeCheck]::Inspect()
$rhythmReport = [ordered]@{
    mode = $(if ($Suspend) { 'explicit-suspend' } else { 'inspect-only' })
    requestedSeconds = $Seconds
    power = $rhythmPower
    availableStates = $rhythmPowerStates
    powerStatesQueryExit = $rhythmStatesExit
    powerRequests = $rhythmPowerRequests
    powerRequestsQueryExit = $rhythmRequestsExit
    forceRequested = $false
    wakeEventsDisabled = $false
    persistentChanges = $false
    result = $null
}

if ($Suspend) {
    if (-not $rhythmPower.CanAttemptTimerWake) {
        $rhythmReport | ConvertTo-Json -Depth 5
        throw 'Refusing sleep: AC power and enabled AC wake timers are required.'
    }
    # If the current token can inspect requests, refuse to ignore an existing
    # blocker. An unavailable elevated-only query is recorded in the evidence.
    if ($rhythmRequestsExit -eq 0) {
        $rhythmRequestEntries = @($rhythmPowerRequests -split "`r?`n" | Where-Object {
            $rhythmLine = $_.Trim()
            $rhythmLine -and $rhythmLine -ne 'None.' -and $rhythmLine -notmatch '^[A-Z_]+:$'
        })
        if ($rhythmRequestEntries.Count -gt 0) {
            $rhythmReport | ConvertTo-Json -Depth 5
            throw 'Refusing sleep while Windows reports an active power request.'
        }
    }
    $rhythmReport.result = [RhythmHardwareSleep.NativeCheck]::SuspendFor($Seconds)
}

$rhythmReport | ConvertTo-Json -Depth 5
if ($rhythmReport.result -and ($rhythmReport.result.Error -or $rhythmReport.result.CleanupError)) {
    throw 'The sleep check returned an error; inspect the JSON evidence above.'
}
