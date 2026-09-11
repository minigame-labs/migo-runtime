import Foundation
import Network

/// The loopback socket the WebContent producer sends frames over, and reads its
/// verdicts and frame-clock ticks back from.
///
/// ## Why a socket at all, and why this one
///
/// The producer is content JavaScript in WebKit's WebContent process; the engine
/// is here. G0's transport probe measured both candidates on a device and put
/// the crossover at 64 KiB: below it a loopback WebSocket round trip is 1.9-2.2x
/// faster than a custom-scheme request and costs 4x less host CPU, above it the
/// scheme request wins by 4.4x. So frames take whichever is cheaper for their
/// size, and **the return path is always this socket** -- a scheme handler
/// answers requests and cannot push, while a verdict and a clock tick are both
/// things the host starts.
///
/// ## One producer
///
/// The listener accepts exactly one connection and refuses the rest. A second
/// producer would submit frames for the same session with its own sequence
/// numbers, and the ingress would reject whichever arrived second as out of
/// order -- a failure that looks like corruption and is a connection that should
/// never have been accepted. Refusing it here is the difference between a clear
/// error and a game that renders every other frame.
///
/// ## What it does not do
///
/// It does not know what a frame is. Bytes arriving are handed to `onFrame`,
/// bytes to send are handed to `send`, and the engine's own encoder produces
/// both -- `migo_session_take_downlink` writes whole downlink messages and this
/// copies them. A transport that built records itself would be a third
/// implementation of a wire format that already has two, which is the drift
/// `scripts/test-render-opcode-agreement.sh` exists to catch elsewhere.
public final class MigoFrameTransport {

    /// Why the transport stopped, or would not start.
    public enum Failure: Error, Equatable {
        /// The listener could not bind a loopback port.
        case listenFailed(String)
        /// The listener never reported a port to publish.
        case noPort
        /// A send was attempted with no producer connected.
        case notConnected
        /// The connection failed or the peer went away.
        case connectionFailed(String)
    }

    /// What the host has to tell the producer so it can connect.
    public struct Endpoint: Equatable, Sendable {
        public let port: UInt16
        /// The literal address, not a hostname: `localhost` does not resolve
        /// inside `WKWebView`, which is a measured fact rather than a style
        /// choice, and `127.0.0.0/8` is potentially trustworthy so the origin
        /// keeps the powers a secure context has.
        public var url: URL { URL(string: "ws://127.0.0.1:\(port)/")! }
    }

    /// Bytes the producer sent. Called on the transport's queue.
    public var onFrame: ((Data) -> Void)?
    /// The producer connected or went away. Called on the transport's queue.
    public var onConnectionChange: ((Bool) -> Void)?
    /// Something went wrong that the host should see. Called on the transport's
    /// queue, and never for the ordinary end of a connection.
    public var onFailure: ((Failure) -> Void)?

    private let queue = DispatchQueue(label: "dev.migo.frame-transport")
    /// Guards `listener` and `connection`, and it is a lock rather than
    /// `queue.sync` on purpose: every Network.framework callback already runs
    /// ON that queue, so a host that called `stop()` or read `isConnected` from
    /// inside `onFrame` -- which is the obvious thing to do -- would deadlock
    /// against itself. A lock is re-entrant-safe here because nothing taken
    /// under it calls back out.
    private let state = NSLock()
    private var listener: NWListener?
    private var connection: NWConnection?

    public init() {}

    /// Bind a loopback port and start listening.
    ///
    /// Returns once the port is known, because the host has nothing to tell the
    /// producer until then and a transport that reported success before it had a
    /// port would hand out a URL that cannot be connected to.
    public func start(timeout: TimeInterval = 5) throws -> Endpoint {
        let parameters = NWParameters.tcp
        // Loopback only. A frame channel that listened on every interface would
        // accept a producer from another machine, which is not a threat model
        // this lane has -- it is a mistake it should be impossible to make.
        //
        // Checked rather than assumed, because `requiredLocalEndpoint` is a
        // request and the consequence of it being ignored would be invisible
        // from inside the process: `lsof -nP -iTCP -sTCP:LISTEN` against a
        // listener started this way reports `TCP 127.0.0.1:<port> (LISTEN)`,
        // IPv4 loopback and nothing else.
        parameters.requiredLocalEndpoint = NWEndpoint.hostPort(host: .ipv4(.loopback), port: .any)
        parameters.allowLocalEndpointReuse = true
        let websocket = NWProtocolWebSocket.Options()
        websocket.autoReplyPing = true
        parameters.defaultProtocolStack.applicationProtocols.insert(websocket, at: 0)

        let listener: NWListener
        do {
            listener = try NWListener(using: parameters)
        } catch {
            throw Failure.listenFailed(String(describing: error))
        }

        let ready = DispatchSemaphore(value: 0)
        var boundPort: UInt16?
        var listenError: Failure?

        listener.stateUpdateHandler = { [weak self] state in
            switch state {
            case .ready:
                boundPort = listener.port?.rawValue
                ready.signal()
            case .failed(let error):
                listenError = .listenFailed(String(describing: error))
                ready.signal()
                self?.onFailure?(.listenFailed(String(describing: error)))
            default:
                break
            }
        }
        listener.newConnectionHandler = { [weak self] connection in
            self?.accept(connection)
        }
        listener.start(queue: queue)
        state.lock()
        self.listener = listener
        state.unlock()

        if ready.wait(timeout: .now() + timeout) == .timedOut {
            stop()
            throw Failure.noPort
        }
        if let listenError {
            stop()
            throw listenError
        }
        guard let boundPort, boundPort != 0 else {
            stop()
            throw Failure.noPort
        }
        // Replaced, not left in place: the handler above closes over locals of
        // this function, and a listener that failed later would write to them
        // after this frame is gone. What is still wanted from it is the report.
        listener.stateUpdateHandler = { [weak self] state in
            if case .failed(let error) = state {
                self?.onFailure?(.listenFailed(String(describing: error)))
            }
        }
        return Endpoint(port: boundPort)
    }

    /// Send one message to the producer.
    ///
    /// `bytes` is a whole downlink message as `migo_session_take_downlink`
    /// wrote it. Empty is not sent: the engine reports "nothing to send" as zero
    /// bytes, and forwarding that as an empty WebSocket message would make the
    /// producer parse an envelope that says nothing.
    public func send(_ bytes: Data) throws {
        guard !bytes.isEmpty else { return }
        state.lock()
        let connection = self.connection
        state.unlock()
        guard let connection, connection.state == .ready else {
            throw Failure.notConnected
        }
        let metadata = NWProtocolWebSocket.Metadata(opcode: .binary)
        let context = NWConnection.ContentContext(identifier: "downlink", metadata: [metadata])
        connection.send(
            content: bytes, contentContext: context, isComplete: true,
            completion: .contentProcessed { [weak self] error in
                if let error {
                    self?.onFailure?(.connectionFailed(String(describing: error)))
                }
            })
    }

    /// Stop listening and drop the producer's connection.
    ///
    /// Safe to call twice, and safe to call from `deinit`: a transport that
    /// threw on a second stop would make every error path a two-step dance.
    public func stop() {
        state.lock()
        let connection = self.connection
        let listener = self.listener
        self.connection = nil
        self.listener = nil
        state.unlock()
        connection?.cancel()
        listener?.newConnectionHandler = nil
        listener?.stateUpdateHandler = nil
        listener?.cancel()
    }

    deinit { stop() }

    /// Whether a producer is connected. For tests and for a host that wants to
    /// report why nothing is rendering.
    public var isConnected: Bool {
        state.lock()
        defer { state.unlock() }
        return connection?.state == .ready
    }

    // MARK: - Private

    private func accept(_ incoming: NWConnection) {
        state.lock()
        let alreadyHaveOne = connection != nil
        if !alreadyHaveOne { connection = incoming }
        state.unlock()
        if alreadyHaveOne {
            // See the type's documentation: a second producer is refused rather
            // than multiplexed.
            incoming.cancel()
            return
        }
        incoming.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            switch state {
            case .ready:
                self.onConnectionChange?(true)
            case .failed(let error):
                self.onFailure?(.connectionFailed(String(describing: error)))
                self.dropConnection()
            case .cancelled:
                self.dropConnection()
            default:
                break
            }
        }
        incoming.start(queue: queue)
        receive(on: incoming)
    }

    private func dropConnection() {
        state.lock()
        let had = connection != nil
        connection = nil
        state.unlock()
        guard had else { return }
        onConnectionChange?(false)
    }

    /// One message at a time, re-armed after each.
    ///
    /// `receiveMessage` rather than `receive(minimumIncompleteLength:)`: the
    /// producer's unit is a whole frame packet, and a partial one is not a
    /// smaller packet -- the validator refuses it. Letting the WebSocket layer
    /// reassemble is what keeps that true without a length prefix of our own.
    private func receive(on connection: NWConnection) {
        connection.receiveMessage { [weak self] data, context, _, error in
            guard let self else { return }
            if let error {
                self.onFailure?(.connectionFailed(String(describing: error)))
                self.dropConnection()
                return
            }
            if let context,
                let metadata = context.protocolMetadata.first(where: {
                    $0 is NWProtocolWebSocket.Metadata
                }) as? NWProtocolWebSocket.Metadata,
                metadata.opcode == .close
            {
                self.dropConnection()
                return
            }
            if let data, !data.isEmpty {
                self.onFrame?(data)
            }
            // Re-arm only while this connection is still the one we accepted:
            // a cancelled connection that kept re-arming would hold the queue
            // alive past `stop()`.
            self.state.lock()
            let stillCurrent = self.connection === connection
            self.state.unlock()
            if stillCurrent {
                self.receive(on: connection)
            }
        }
    }
}
