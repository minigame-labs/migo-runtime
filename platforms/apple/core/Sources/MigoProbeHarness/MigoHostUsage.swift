import Darwin
import Foundation

/// What this process has spent, sampled cheaply enough to bracket a batch.
///
/// A32's threshold has two halves and only one of them was measurable from
/// latency: *"需预先声明阈值（例：合计 CPU 5% 或 p99 0.25 ms）才算赢"*. The p99 half
/// is in the transport record already. This is the other half.
///
/// It matters because the two channels have different shapes of cost. A
/// WebSocket is one connection and a frame's worth of framing per message; a
/// custom-scheme request is a task constructed, dispatched and torn down per
/// call. At sixty calls a second the second shape can lose on CPU while looking
/// level on latency, which is exactly what A32 says to check before declaring a
/// winner.
///
/// 🔴 **THIS IS THE HOST'S HALF AND NOT THE WHOLE.** The page's own work --
/// JavaScript, WebKit's framing, the response buffer -- happens in the
/// WebContent process, and iOS gives an app no public way to read another
/// process's CPU. So a comparison built on this measures what the host spends
/// serving each transport, which is what A32 asks about, and says nothing about
/// what WebContent spends using it. Reporting it as a total would be the
/// measurement claiming a reach it does not have.
public struct MigoHostUsage: Equatable, Sendable {
    /// User plus system time, live threads plus threads that have already
    /// exited. Both halves are needed: `TASK_THREAD_TIMES_INFO` counts only
    /// threads that still exist, so a batch that spawned and joined one would
    /// show as costing nothing.
    public var cpuMilliseconds: Double

    /// Interrupt wakeups, which is the number a power budget is written
    /// against. A transport that costs little CPU by waking the process
    /// constantly has not won anything.
    public var wakeups: UInt64

    public init(cpuMilliseconds: Double, wakeups: UInt64) {
        self.cpuMilliseconds = cpuMilliseconds
        self.wakeups = wakeups
    }

    /// Reads the counters, or nil when the kernel declines.
    ///
    /// Nil rather than zero: a zero delta is a legitimate answer for a batch
    /// that did almost nothing, and a failed read reported as zero would be
    /// indistinguishable from it.
    public static func sample() -> MigoHostUsage? {
        var live = task_thread_times_info_data_t()
        var liveCount = mach_msg_type_number_t(
            MemoryLayout<task_thread_times_info_data_t>.size / MemoryLayout<natural_t>.size)
        let liveResult = withUnsafeMutablePointer(to: &live) { pointer in
            pointer.withMemoryRebound(to: integer_t.self, capacity: Int(liveCount)) {
                task_info(mach_task_self_, task_flavor_t(TASK_THREAD_TIMES_INFO), $0, &liveCount)
            }
        }
        guard liveResult == KERN_SUCCESS else { return nil }

        var terminated = task_basic_info_64_data_t()
        var terminatedCount = mach_msg_type_number_t(
            MemoryLayout<task_basic_info_64_data_t>.size / MemoryLayout<natural_t>.size)
        let terminatedResult = withUnsafeMutablePointer(to: &terminated) { pointer in
            pointer.withMemoryRebound(to: integer_t.self, capacity: Int(terminatedCount)) {
                task_info(mach_task_self_, task_flavor_t(TASK_BASIC_INFO_64), $0, &terminatedCount)
            }
        }
        guard terminatedResult == KERN_SUCCESS else { return nil }

        var power = task_power_info_data_t()
        var powerCount = mach_msg_type_number_t(
            MemoryLayout<task_power_info_data_t>.size / MemoryLayout<natural_t>.size)
        let powerResult = withUnsafeMutablePointer(to: &power) { pointer in
            pointer.withMemoryRebound(to: integer_t.self, capacity: Int(powerCount)) {
                task_info(mach_task_self_, task_flavor_t(TASK_POWER_INFO), $0, &powerCount)
            }
        }

        let milliseconds =
            Self.milliseconds(live.user_time) + Self.milliseconds(live.system_time)
            + Self.milliseconds(terminated.user_time) + Self.milliseconds(terminated.system_time)
        return MigoHostUsage(
            cpuMilliseconds: milliseconds,
            // Zero when the power flavour is refused, which is honest: the CPU
            // half was read and the wakeup half was not, and a record that
            // dropped both would lose the half that worked.
            wakeups: powerResult == KERN_SUCCESS ? power.task_interrupt_wakeups : 0)
    }

    private static func milliseconds(_ value: time_value_t) -> Double {
        Double(value.seconds) * 1000.0 + Double(value.microseconds) / 1000.0
    }
}
