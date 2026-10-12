import CoreHaptics
import Foundation
import GameController
import MigoEngine

/// The game controllers the system knows, as content's gamepads
/// (`getGamepads`, `onGamepadConnected`): each pad with an extended profile --
/// Xbox, PlayStation, MFi -- announced in the lowest free slot once the
/// session has a surface, mapped onto the standard layout, and sampled
/// whenever one of its elements changes. Its motors play content's
/// vibrationActuator effects through CoreHaptics, the strong effect on the
/// left handle and the weak one on the right where the pad has two, and the
/// trigger effects on the triggers of a pad that has motors there.
///
/// Main thread, like the session it reports to.
final class MigoGamepads {
    private weak var session: MigoEngineSession?
    /// The pad in each slot; a slot a pad left stays empty until the next
    /// arrives, since content holds on to an index.
    private var slots: [Pad?] = []
    private var observers: [NSObjectProtocol] = []
    private var running = false

    init(session: MigoEngineSession) {
        self.session = session
    }

    deinit {
        observers.forEach(NotificationCenter.default.removeObserver)
    }

    /// The session has a surface: announce the pads there are and follow the
    /// rest. Again after a later attach, which is a new surface to announce to.
    func start() {
        if running {
            for (index, pad) in slots.enumerated() { if let pad { announce(pad, at: index) } }
            return
        }
        running = true
        let center = NotificationCenter.default
        observers.append(
            center.addObserver(forName: .GCControllerDidConnect, object: nil, queue: .main) { [weak self] note in
                (note.object as? GCController).map { self?.connect($0) }
            })
        observers.append(
            center.addObserver(forName: .GCControllerDidDisconnect, object: nil, queue: .main) { [weak self] note in
                (note.object as? GCController).map { self?.disconnect($0) }
            })
        GCController.controllers().forEach(connect)
    }

    /// The session closes: nothing more is sent, and the motors stop.
    func stop() {
        running = false
        observers.forEach(NotificationCenter.default.removeObserver)
        observers = []
        for pad in slots.compactMap({ $0 }) { pad.release() }
        slots = []
    }

    private func connect(_ controller: GCController) {
        guard running, let gamepad = controller.extendedGamepad,
            !slots.contains(where: { $0?.controller === controller })
        else { return }
        let pad = Pad(controller: controller)
        let index = Self.slot(in: slots.map { $0 != nil })
        if index == slots.count { slots.append(pad) } else { slots[index] = pad }
        announce(pad, at: index)
        gamepad.valueChangedHandler = { [weak self, weak pad] gamepad, _ in
            guard let self, let pad, let index = self.slots.firstIndex(where: { $0 === pad }) else { return }
            self.send(Self.snapshot(of: gamepad), at: index)
        }
    }

    private func disconnect(_ controller: GCController) {
        guard let index = slots.firstIndex(where: { $0?.controller === controller }), let pad = slots[index] else {
            return
        }
        pad.release()
        slots[index] = nil
        var info = MigoGamepadInfo()
        info.struct_size = UInt32(MemoryLayout<MigoGamepadInfo>.size)
        info.abi_version = MIGO_ABI_VERSION_CURRENT
        info.index = UInt32(index)
        session?.setGamepad(info, connected: false)
    }

    private func announce(_ pad: Pad, at index: Int) {
        let name = pad.controller.vendorName ?? pad.controller.productCategory
        name.withCString { id in
            "standard".withCString { mapping in
                var info = MigoGamepadInfo()
                info.struct_size = UInt32(MemoryLayout<MigoGamepadInfo>.size)
                info.abi_version = MIGO_ABI_VERSION_CURRENT
                info.index = UInt32(index)
                info.axis_count = UInt32(Self.axisCount)
                info.button_count = UInt32(Self.buttonCount)
                info.haptics = pad.haptics
                info.id_utf8 = id
                info.mapping_utf8 = mapping
                session?.setGamepad(info, connected: true)
            }
        }
        if let gamepad = pad.controller.extendedGamepad { send(Self.snapshot(of: gamepad), at: index) }
    }

    private func send(_ snapshot: Snapshot, at index: Int) {
        guard running else { return }
        session?.sendGamepadState(
            index: UInt32(index), axes: snapshot.axes, buttons: snapshot.buttons,
            timestampMilliseconds: ProcessInfo.processInfo.systemUptime * 1000)
    }

    /// Content's effect for the pad in `rumble.index`.
    func rumble(_ rumble: MigoGamepadRumble) {
        guard slots.indices.contains(Int(rumble.index)), let pad = slots[Int(rumble.index)] else { return }
        pad.play(rumble)
    }

    // MARK: - the standard layout

    static let axisCount = 4
    /// Sixteen buttons and the home button.
    static let buttonCount = 17

    struct Snapshot: Equatable {
        var axes: [Float]
        var buttons: [MigoGamepadButton]

        static func == (a: Snapshot, b: Snapshot) -> Bool {
            a.axes == b.axes
                && a.buttons.map { [Float($0.flags), $0.value] } == b.buttons.map { [Float($0.flags), $0.value] }
        }
    }

    /// The lowest slot no pad holds.
    static func slot(in taken: [Bool]) -> Int {
        taken.firstIndex(of: false) ?? taken.count
    }

    /// `gamepad` in the standard layout: the face buttons bottom, right, left,
    /// top; shoulders; triggers; back and start; the sticks' buttons; the pad
    /// up, down, left, right; home -- and the sticks' axes, down and right
    /// positive.
    static func snapshot(of gamepad: GCExtendedGamepad) -> Snapshot {
        func button(_ input: GCControllerButtonInput?) -> MigoGamepadButton {
            guard let input else { return MigoGamepadButton() }
            var flags = MigoGamepadButtonFlags(MIGO_GAMEPAD_BUTTON_FLAG_NONE)
            if input.isPressed { flags |= MigoGamepadButtonFlags(MIGO_GAMEPAD_BUTTON_FLAG_PRESSED) }
            if input.isPressed || input.value > 0 { flags |= MigoGamepadButtonFlags(MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED) }
            return MigoGamepadButton(flags: flags, value: input.value)
        }
        let buttons = [
            button(gamepad.buttonA), button(gamepad.buttonB), button(gamepad.buttonX), button(gamepad.buttonY),
            button(gamepad.leftShoulder), button(gamepad.rightShoulder),
            button(gamepad.leftTrigger), button(gamepad.rightTrigger),
            button(gamepad.buttonOptions), button(gamepad.buttonMenu),
            button(gamepad.leftThumbstickButton), button(gamepad.rightThumbstickButton),
            button(gamepad.dpad.up), button(gamepad.dpad.down), button(gamepad.dpad.left), button(gamepad.dpad.right),
            button(gamepad.buttonHome),
        ]
        let axes = [
            gamepad.leftThumbstick.xAxis.value, -gamepad.leftThumbstick.yAxis.value,
            gamepad.rightThumbstick.xAxis.value, -gamepad.rightThumbstick.yAxis.value,
        ]
        return Snapshot(axes: axes, buttons: buttons)
    }

    /// The effects a pad's motors can play.
    static func haptics(for localities: Set<GCHapticsLocality>) -> MigoGamepadHaptics {
        var haptics = MigoGamepadHaptics(MIGO_GAMEPAD_HAPTICS_NONE)
        let handles: Set<GCHapticsLocality> = [.handles, .leftHandle, .rightHandle, .default, .all]
        if !localities.isDisjoint(with: handles) { haptics |= MigoGamepadHaptics(MIGO_GAMEPAD_HAPTICS_DUAL_RUMBLE) }
        if localities.contains(.triggers) || localities.isSuperset(of: [.leftTrigger, .rightTrigger]) {
            haptics |= MigoGamepadHaptics(MIGO_GAMEPAD_HAPTICS_TRIGGER_RUMBLE)
        }
        return haptics
    }

    // MARK: - one pad

    private final class Pad {
        let controller: GCController
        let haptics: MigoGamepadHaptics
        /// An engine and what it plays now, per motor, made when first used.
        private var motors: [GCHapticsLocality: (engine: CHHapticEngine, player: CHHapticPatternPlayer?)] = [:]

        init(controller: GCController) {
            self.controller = controller
            haptics = MigoGamepads.haptics(for: controller.haptics?.supportedLocalities ?? [])
        }

        func release() {
            controller.extendedGamepad?.valueChangedHandler = nil
            for motor in motors.values {
                try? motor.player?.stop(atTime: CHHapticTimeImmediate)
                motor.engine.stop()
            }
            motors = [:]
        }

        func play(_ rumble: MigoGamepadRumble) {
            let seconds = TimeInterval(rumble.duration_ms) / 1000
            let localities = controller.haptics?.supportedLocalities ?? []
            var levels: [GCHapticsLocality: Float] = [:]
            if localities.isSuperset(of: [.leftHandle, .rightHandle]) {
                levels[.leftHandle] = rumble.strong_magnitude
                levels[.rightHandle] = rumble.weak_magnitude
            } else if let single = [GCHapticsLocality.handles, .default, .all].first(where: localities.contains) {
                // One motor for both: the larger of the two.
                levels[single] = max(rumble.strong_magnitude, rumble.weak_magnitude)
            }
            if haptics & MigoGamepadHaptics(MIGO_GAMEPAD_HAPTICS_TRIGGER_RUMBLE) != 0 {
                if localities.isSuperset(of: [.leftTrigger, .rightTrigger]) {
                    levels[.leftTrigger] = rumble.left_trigger
                    levels[.rightTrigger] = rumble.right_trigger
                } else {
                    levels[.triggers] = max(rumble.left_trigger, rumble.right_trigger)
                }
            }
            for (locality, level) in levels { drive(locality, level: rumble.duration_ms == 0 ? 0 : level, for: seconds) }
        }

        /// Replace what the motor at `locality` plays with `level` for
        /// `seconds`; 0 stops it.
        private func drive(_ locality: GCHapticsLocality, level: Float, for seconds: TimeInterval) {
            if let player = motors[locality]?.player {
                try? player.stop(atTime: CHHapticTimeImmediate)
                motors[locality]?.player = nil
            }
            guard level > 0, seconds > 0 else { return }
            do {
                let engine: CHHapticEngine
                if let existing = motors[locality]?.engine {
                    engine = existing
                } else {
                    guard let made = controller.haptics?.createEngine(withLocality: locality) else { return }
                    engine = made
                }
                try engine.start()
                let event = CHHapticEvent(
                    eventType: .hapticContinuous,
                    parameters: [CHHapticEventParameter(parameterID: .hapticIntensity, value: min(max(level, 0), 1))],
                    relativeTime: 0, duration: seconds)
                let player = try engine.makePlayer(with: CHHapticPattern(events: [event], parameters: []))
                try player.start(atTime: CHHapticTimeImmediate)
                motors[locality] = (engine, player)
            } catch {
                NSLog("MigoGamepads: the pad's motor did not play: \(error)")
            }
        }
    }
}
