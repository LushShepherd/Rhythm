# Windows lifecycle adapter

`WindowsClock` uses `QueryUnbiasedInterruptTimePrecise` for timer credit. This
counter excludes sleep and hibernation. Wall-clock corrections cannot add or
remove timer credit. No timer-resolution or system power settings are changed.

Every process creates one random epoch with an interruption generation. Native
suspend changes that generation before sending its event. `interruption_generation()`
also checks paired awake/inclusive elapsed clocks, detecting a sleep gap even if
callbacks are late or unavailable. The changed epoch protects a timer action that
races the worker's lifecycle check. The desktop closes the interrupted period at
its last durable elapsed checkpoint and returns to untimed Break, waiting for an
explicit new Focus. It does the same on reopening after shutdown or a crash.
The adapter does not infer activity from UTC or claim a verified boot identity.
The non-Windows build reports `PortableProcessOnly`; it uses `Instant` for development
and does not establish a native macOS/Linux sleep implementation.

`PowerSubscription::subscribe()` returns an RAII guard and bounded event receiver.
Keep the guard alive and drain the receiver from the worker. Before processing
timer actions or polling, compare the clock's interruption generation with the
last handled value and apply `Command::Interrupt` when it changes. Check
`is_suspended()` to prevent new periods before resume. The callbacks latch state
atomically even when the queue is full. Duplicate resume signals do not increment
the generation. A detected clock gap can clear a suspended flag if the resume
callback was lost. Neither callback delivery nor clock-gap detection starts a period.

Clock-gap detection brackets `GetTickCount64` with two awake-clock reads. Their
difference bounds tolerate thread preemption, while a 50 ms allowance covers the
inclusive counter's tick quantization. Existing sleep history at first sampling
is only a baseline. Ordinary awake worker delays and UTC changes do not count as
sleep. Native callbacks detect interruptions too short for that fallback tolerance.

The Windows callback has no heap context pointer. It accesses a static optional
channel using `try_lock` and `try_send`, never calling storage or user code. Only
one subscription is allowed per process. Drop unregisters it; `close()` reports an
unregister failure when explicit shutdown diagnostics are needed. If Windows
rejects unregistering, any retained OS registration lasts until process exit but
cannot access freed application data. All unsafe code is confined to `src/native.rs`, with documented FFI
preconditions; the rest of this crate denies unsafe code.

The deterministic integration suite uses the real application service and SQLite
to cover sleep before/after a deadline, duplicate interruption, paused periods,
wall-clock correction, and explicit new Focus after restart. Pure detector tests
cover missed callbacks, scheduling delays, native/fallback deduplication, and a
cancelled suspend followed by an unreported sleep. Windows tests read the native
clock and simulate callbacks, including a full queue. They never suspend the host.
The optional hardware observer has also been updated for interruption semantics;
it is not run by ordinary tests and supplies no current physical-device evidence.

Primary API references:

- [Microsoft: GetTickCount64](https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-gettickcount64)
- [Microsoft: QueryUnbiasedInterruptTimePrecise](https://learn.microsoft.com/en-us/windows/win32/api/realtimeapiset/nf-realtimeapiset-queryunbiasedinterrupttimeprecise)
- [Microsoft: Interrupt time, including sleep/hibernation guidance](https://learn.microsoft.com/en-us/windows/win32/sysinfo/interrupt-time)
- [Microsoft: PowerRegisterSuspendResumeNotification](https://learn.microsoft.com/en-us/windows/win32/api/powerbase/nf-powerbase-powerregistersuspendresumenotification)
- [Microsoft: DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS](https://learn.microsoft.com/en-us/windows/win32/api/powrprof/ns-powrprof-device_notify_subscribe_parameters)
- [Microsoft: callback contract](https://learn.microsoft.com/en-us/windows/win32/api/powrprof/nc-powrprof-device_notify_callback_routine)
- [Microsoft: PowerUnregisterSuspendResumeNotification](https://learn.microsoft.com/en-us/windows/win32/api/powerbase/nf-powerbase-powerunregistersuspendresumenotification)
