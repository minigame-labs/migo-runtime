import Foundation

#if os(macOS)
    import AppKit

    /// The system's dialogs on a Mac: a modal and an action sheet are alerts
    /// shown as sheets on the app's window, and a toast or a loading indicator
    /// is a heads-up panel over the window's content.
    final class MigoSystemInteractionPresenter: MigoInteractionPresenter {
        private let window: () -> NSWindow?
        private var hud: NSView?

        /// `window` is where the dialogs go: the app's key window by default.
        init(window: @escaping () -> NSWindow? = { NSApp.keyWindow ?? NSApp.mainWindow }) {
            self.window = window
        }

        func showHud(_ hud: MigoHud) {
            hideHud()
            guard let content = window()?.contentView else { return }
            let overlay = MigoHudOverlay(hud: hud)
            overlay.frame = content.bounds
            overlay.autoresizingMask = [.width, .height]
            content.addSubview(overlay, positioned: .above, relativeTo: nil)
            self.hud = overlay
        }

        func hideHud() {
            hud?.removeFromSuperview()
            hud = nil
        }

        func showModal(_ modal: MigoModal, completion: @escaping (MigoModalAnswer?) -> Void) {
            guard let window = window() else {
                completion(nil)
                return
            }
            let alert = NSAlert()
            alert.messageText = modal.title
            alert.addButton(withTitle: modal.confirmText)
            if modal.showCancel {
                alert.addButton(withTitle: modal.cancelText).keyEquivalent = "\u{1b}"
            }
            var input: NSTextField?
            if modal.editable {
                let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
                field.stringValue = modal.content
                field.placeholderString = modal.placeholderText
                alert.accessoryView = field
                input = field
            } else {
                alert.informativeText = modal.content
            }
            alert.beginSheetModal(for: window) { response in
                completion(
                    MigoModalAnswer(confirmed: response == .alertFirstButtonReturn, content: input?.stringValue))
            }
            if let input { alert.window.initialFirstResponder = input }
        }

        func showActionSheet(title: String, items: [String], completion: @escaping (Int?) -> Void) {
            guard let window = window() else {
                completion(nil)
                return
            }
            let alert = NSAlert()
            alert.messageText = title
            for item in items { alert.addButton(withTitle: item) }
            alert.addButton(withTitle: MigoSystemInteraction.cancel).keyEquivalent = "\u{1b}"
            alert.beginSheetModal(for: window) { response in
                let index = response.rawValue - NSApplication.ModalResponse.alertFirstButtonReturn.rawValue
                completion(index >= 0 && index < items.count ? index : -1)
            }
        }
    }

    /// A toast or a loading indicator: a small dark panel in the middle of the
    /// window. With `mask` it also covers the window, so the game below gets no
    /// clicks; without, clicks go straight through.
    final class MigoHudOverlay: NSView {
        private let masks: Bool

        init(hud: MigoHud) {
            masks = hud.mask
            super.init(frame: .zero)
            let panel = NSVisualEffectView()
            panel.material = .hudWindow
            panel.blendingMode = .withinWindow
            panel.state = .active
            panel.wantsLayer = true
            panel.layer?.cornerRadius = 10
            panel.translatesAutoresizingMaskIntoConstraints = false

            let stack = NSStackView()
            stack.orientation = .vertical
            stack.spacing = 8
            stack.edgeInsets = NSEdgeInsets(top: 16, left: 20, bottom: 16, right: 20)
            stack.translatesAutoresizingMaskIntoConstraints = false
            if let symbol = Self.symbol(for: hud.kind) {
                if symbol == "loading" {
                    let spinner = NSProgressIndicator()
                    spinner.style = .spinning
                    spinner.controlSize = .regular
                    spinner.startAnimation(nil)
                    stack.addArrangedSubview(spinner)
                } else if let image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil) {
                    let view = NSImageView(image: image)
                    view.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 28, weight: .regular)
                    view.contentTintColor = .white
                    stack.addArrangedSubview(view)
                }
            }
            if !hud.title.isEmpty {
                let label = NSTextField(labelWithString: hud.title)
                label.textColor = .white
                label.alignment = .center
                label.maximumNumberOfLines = 3
                label.preferredMaxLayoutWidth = 220
                stack.addArrangedSubview(label)
            }
            panel.addSubview(stack)
            addSubview(panel)
            NSLayoutConstraint.activate([
                stack.leadingAnchor.constraint(equalTo: panel.leadingAnchor),
                stack.trailingAnchor.constraint(equalTo: panel.trailingAnchor),
                stack.topAnchor.constraint(equalTo: panel.topAnchor),
                stack.bottomAnchor.constraint(equalTo: panel.bottomAnchor),
                panel.centerXAnchor.constraint(equalTo: centerXAnchor),
                panel.centerYAnchor.constraint(equalTo: centerYAnchor),
                panel.widthAnchor.constraint(greaterThanOrEqualToConstant: 120),
            ])
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) { nil }

        /// The icon a toast shows, `"loading"` for the spinner, `nil` for none.
        static func symbol(for kind: MigoHud.Kind) -> String? {
            switch kind {
            case .loading: return "loading"
            case .toast(let icon):
                switch icon {
                case "success": return "checkmark.circle"
                case "error": return "xmark.circle"
                case "loading": return "loading"
                default: return nil
                }
            }
        }

        override func hitTest(_ point: NSPoint) -> NSView? {
            masks ? (super.hitTest(point) ?? self) : nil
        }

        override func mouseDown(with event: NSEvent) {}
        override func rightMouseDown(with event: NSEvent) {}
        override func scrollWheel(with event: NSEvent) {}
    }

#elseif os(iOS)
    import UIKit

    /// The system's dialogs on iOS: alerts and action sheets presented from the
    /// top view controller of the key window, and a toast or a loading
    /// indicator as a heads-up view over the window.
    final class MigoSystemInteractionPresenter: MigoInteractionPresenter {
        private var hud: UIView?

        init() {}

        private func keyWindow() -> UIWindow? {
            UIApplication.shared.connectedScenes
                .compactMap { $0 as? UIWindowScene }
                .flatMap(\.windows)
                .first { $0.isKeyWindow }
        }

        private func topController() -> UIViewController? {
            var top = keyWindow()?.rootViewController
            while let presented = top?.presentedViewController { top = presented }
            return top
        }

        func showHud(_ hud: MigoHud) {
            hideHud()
            guard let window = keyWindow() else { return }
            let overlay = MigoHudOverlay(hud: hud)
            overlay.frame = window.bounds
            overlay.autoresizingMask = [.flexibleWidth, .flexibleHeight]
            window.addSubview(overlay)
            self.hud = overlay
        }

        func hideHud() {
            hud?.removeFromSuperview()
            hud = nil
        }

        func showModal(_ modal: MigoModal, completion: @escaping (MigoModalAnswer?) -> Void) {
            guard let controller = topController() else {
                completion(nil)
                return
            }
            let alert = UIAlertController(
                title: modal.title.isEmpty ? nil : modal.title,
                message: modal.editable || modal.content.isEmpty ? nil : modal.content,
                preferredStyle: .alert)
            if modal.editable {
                alert.addTextField { field in
                    field.text = modal.content
                    field.placeholder = modal.placeholderText
                }
            }
            if modal.showCancel {
                alert.addAction(
                    UIAlertAction(title: modal.cancelText, style: .cancel) { _ in
                        completion(MigoModalAnswer(confirmed: false, content: alert.textFields?.first?.text))
                    })
            }
            alert.addAction(
                UIAlertAction(title: modal.confirmText, style: .default) { _ in
                    completion(MigoModalAnswer(confirmed: true, content: alert.textFields?.first?.text))
                })
            controller.present(alert, animated: true)
        }

        func showActionSheet(title: String, items: [String], completion: @escaping (Int?) -> Void) {
            guard let controller = topController() else {
                completion(nil)
                return
            }
            let sheet = UIAlertController(
                title: title.isEmpty ? nil : title, message: nil, preferredStyle: .actionSheet)
            for (index, item) in items.enumerated() {
                sheet.addAction(UIAlertAction(title: item, style: .default) { _ in completion(index) })
            }
            sheet.addAction(
                UIAlertAction(title: MigoSystemInteraction.cancel, style: .cancel) { _ in completion(-1) })
            // An iPad shows a sheet as a popover, which needs somewhere to point.
            if let popover = sheet.popoverPresentationController {
                popover.sourceView = controller.view
                popover.sourceRect = CGRect(x: controller.view.bounds.midX, y: controller.view.bounds.midY, width: 0, height: 0)
                popover.permittedArrowDirections = []
            }
            controller.present(sheet, animated: true)
        }
    }

    /// A toast or a loading indicator: a small dark panel in the middle of the
    /// window. With `mask` it also covers the window, so the game below gets no
    /// touches; without, touches go straight through.
    final class MigoHudOverlay: UIView {
        init(hud: MigoHud) {
            super.init(frame: .zero)
            isUserInteractionEnabled = hud.mask
            let panel = UIVisualEffectView(effect: UIBlurEffect(style: .systemMaterialDark))
            panel.layer.cornerRadius = 10
            panel.clipsToBounds = true
            panel.translatesAutoresizingMaskIntoConstraints = false
            let stack = UIStackView()
            stack.axis = .vertical
            stack.alignment = .center
            stack.spacing = 8
            stack.isLayoutMarginsRelativeArrangement = true
            stack.layoutMargins = UIEdgeInsets(top: 16, left: 20, bottom: 16, right: 20)
            stack.translatesAutoresizingMaskIntoConstraints = false
            switch Self.symbol(for: hud.kind) {
            case "loading"?:
                let spinner = UIActivityIndicatorView(style: .large)
                spinner.color = .white
                spinner.startAnimating()
                stack.addArrangedSubview(spinner)
            case let name?:
                let view = UIImageView(image: UIImage(systemName: name))
                view.tintColor = .white
                view.preferredSymbolConfiguration = UIImage.SymbolConfiguration(pointSize: 28)
                stack.addArrangedSubview(view)
            case nil:
                break
            }
            if !hud.title.isEmpty {
                let label = UILabel()
                label.text = hud.title
                label.textColor = .white
                label.textAlignment = .center
                label.numberOfLines = 3
                stack.addArrangedSubview(label)
            }
            panel.contentView.addSubview(stack)
            addSubview(panel)
            NSLayoutConstraint.activate([
                stack.leadingAnchor.constraint(equalTo: panel.contentView.leadingAnchor),
                stack.trailingAnchor.constraint(equalTo: panel.contentView.trailingAnchor),
                stack.topAnchor.constraint(equalTo: panel.contentView.topAnchor),
                stack.bottomAnchor.constraint(equalTo: panel.contentView.bottomAnchor),
                panel.centerXAnchor.constraint(equalTo: centerXAnchor),
                panel.centerYAnchor.constraint(equalTo: centerYAnchor),
                panel.widthAnchor.constraint(greaterThanOrEqualToConstant: 120),
                panel.widthAnchor.constraint(lessThanOrEqualToConstant: 260),
            ])
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) { nil }

        /// The icon a toast shows, `"loading"` for the spinner, `nil` for none.
        static func symbol(for kind: MigoHud.Kind) -> String? {
            switch kind {
            case .loading: return "loading"
            case .toast(let icon):
                switch icon {
                case "success": return "checkmark.circle"
                case "error": return "xmark.circle"
                case "loading": return "loading"
                default: return nil
                }
            }
        }
    }
#endif
