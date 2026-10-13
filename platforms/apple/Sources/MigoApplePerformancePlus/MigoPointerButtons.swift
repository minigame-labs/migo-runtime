#if os(iOS)
    import UIKit

    /// The mouse or trackpad buttons held over the game, in DOM
    /// `MouseEvent.button` numbers -- 0 primary, 1 middle, 2 secondary, 3 back,
    /// 4 forward -- and what changed when a source reports them.
    ///
    /// Two sources report the same buttons: UIKit, with each pointer event's
    /// button mask, and GameController's mouse, button by button, while the
    /// pointer is locked and UIKit's pointer stands still. Each press is reported
    /// once whichever source says it first, and a release once.
    struct MigoPointerButtons {
        private(set) var held: Set<UInt32> = []

        /// The buttons a UIKit button mask holds: bit `n` is HID button `n + 1`,
        /// and HID numbers the middle button third where the DOM numbers it
        /// second.
        static func buttons(in mask: UIEvent.ButtonMask) -> Set<UInt32> {
            var buttons: Set<UInt32> = []
            let raw = mask.rawValue
            for (bit, button) in [(0, 0), (1, 2), (2, 1), (3, 3), (4, 4)] where raw & (1 << bit) != 0 {
                buttons.insert(UInt32(button))
            }
            return buttons
        }

        /// Hold exactly `buttons`: the releases, then the presses, that make it so.
        mutating func hold(_ buttons: Set<UInt32>) -> (released: [UInt32], pressed: [UInt32]) {
            let released = held.subtracting(buttons).sorted()
            let pressed = buttons.subtracting(held).sorted()
            held = buttons
            return (released, pressed)
        }

        /// One button pressed or released; whether that changed anything.
        mutating func set(_ button: UInt32, down: Bool) -> Bool {
            down ? held.insert(button).inserted : held.remove(button) != nil
        }

        /// The button a move reports while buttons are held, primary first, as
        /// the Android SDK reports it; 0 when none is.
        var moving: UInt32 { [0, 1, 2, 3, 4].first(where: held.contains) ?? 0 }
    }
#endif
