import Foundation
import MigoEngine

/// A service the host app supplies through the engine's host-service channel:
/// ads, payments, sign-in, sharing, navigation, the system's dialogs and
/// pickers -- what content asks of the app rather than of the engine. The
/// numbers are `contracts/runtime/host-services.json`'s, as
/// `include/migo/host_services.h` defines them; that header documents each
/// service's methods and the JSON they carry.
public struct MigoHostServiceID: Hashable, Sendable, CustomStringConvertible {
    public let rawValue: UInt32

    public init(rawValue: UInt32) {
        self.rawValue = rawValue
    }

    // The services themselves, `MigoHostServiceID.clipboard` and the rest, with
    // each one's method and event numbers, are generated from the contract:
    // MigoHostServiceNumbers.swift.

    public var description: String { "MigoHostServiceID(\(rawValue))" }

    /// The `host_services` bit that declares the service.
    var bit: UInt64 { 1 << UInt64(rawValue) }
}

/// One request content made of the host app.
public struct MigoHostServiceRequest {
    public let service: MigoHostServiceID
    /// The method within the service: a `MIGO_<SERVICE>_<METHOD>` number.
    public let method: UInt32
    /// The request as the JSON object the header documents for the method.
    public let payload: Data
    /// How the request is answered -- `nil` for a command, which takes no
    /// answer.
    public let reply: MigoHostServiceReply?

    public init(service: MigoHostServiceID, method: UInt32, payload: Data, reply: MigoHostServiceReply?) {
        self.service = service
        self.method = method
        self.payload = payload
        self.reply = reply
    }

    /// The payload decoded; empty when it is not a JSON object.
    public var object: [String: Any] {
        (try? JSONSerialization.jsonObject(with: payload)) as? [String: Any] ?? [:]
    }
}

/// The answer to one call, given exactly once -- now, or whenever the app has
/// it (a dialog the player answers, a payment the store settles). From any
/// thread.
///
/// A call nobody answers would leave content waiting for ever, so a reply
/// dropped unanswered answers itself, as a failure.
public final class MigoHostServiceReply: @unchecked Sendable {
    enum Outcome: Equatable {
        case success(json: Data)
        case failure(message: String, code: Int32?)
    }

    private let deliver: (Outcome) -> Void
    private let lock = NSLock()
    private var answered = false

    init(deliver: @escaping (Outcome) -> Void) {
        self.deliver = deliver
    }

    deinit {
        if !answered {
            deliver(.failure(message: "the host did not answer", code: nil))
        }
    }

    /// The call succeeded with `result`, the method's result object.
    public func succeed(_ result: [String: Any] = [:]) {
        guard JSONSerialization.isValidJSONObject(result),
            let json = try? JSONSerialization.data(withJSONObject: result)
        else {
            fail("the host's result is not JSON")
            return
        }
        answer(.success(json: json))
    }

    /// The call succeeded with `json`, a JSON object the app encoded itself.
    public func succeed(json: Data) {
        answer(.success(json: json))
    }

    /// The call failed for `message` -- content sees `<api>:fail <message>` --
    /// with the API's numeric `code` where it defines one. A player who
    /// dismissed what was asked is the message `"cancel"`.
    public func fail(_ message: String, code: Int32? = nil) {
        answer(.failure(message: message.isEmpty ? "failed" : message, code: code))
    }

    private func answer(_ outcome: Outcome) {
        lock.lock()
        let first = !answered
        answered = true
        lock.unlock()
        guard first else {
            NSLog("MigoHostServiceReply: a call was answered twice; the second answer is ignored")
            return
        }
        deliver(outcome)
    }
}

/// What the host app supplies of the host-service channel.
///
/// The services it names are declared to the engine when the session starts,
/// and content's requests of those -- and only those -- reach `handle`. A
/// service it does not name is one the app does not offer, and content is told
/// so. On the main thread.
public protocol MigoHostServiceProvider: AnyObject {
    /// The services this provider answers.
    var services: Set<MigoHostServiceID> { get }

    /// One request. A call is answered exactly once through `request.reply`.
    func handle(_ request: MigoHostServiceRequest)

    /// Whether this provider answers `request`: by default every request of a
    /// service it names. One that answers only some of a service's methods
    /// says which here, and the rest go on to whoever answers the service --
    /// the SDK's own, say.
    func answers(_ request: MigoHostServiceRequest) -> Bool
}

extension MigoHostServiceProvider {
    public func answers(_ request: MigoHostServiceRequest) -> Bool {
        services.contains(request.service)
    }
}

extension MigoEngineSession {
    /// Post one of a service's events -- a `MIGO_<SERVICE>_EVENT_*` number with
    /// the JSON object the header documents for it -- to content. Main thread.
    public func postHostServiceEvent(_ service: MigoHostServiceID, event: UInt32, json: Data) {
        guard !isClosed else { return }
        let result = json.withUnsafeBytes { bytes -> MigoResult in
            migo_session_post_host_service_event(
                session, service.rawValue, event, bytes.bindMemory(to: CChar.self).baseAddress,
                UInt32(bytes.count))
        }
        if result != MIGO_OK {
            NSLog("MigoEngineSession: migo_session_post_host_service_event returned \(result)")
        }
    }

    /// Hand a call's outcome to the engine. Main thread; nothing once closed.
    func completeHostServiceCall(_ callId: UInt64, _ outcome: MigoHostServiceReply.Outcome) {
        guard !isClosed else { return }
        var result = MigoHostServiceResult()
        result.struct_size = UInt32(MemoryLayout<MigoHostServiceResult>.size)
        result.abi_version = MIGO_ABI_VERSION_CURRENT
        let status: MigoResult
        switch outcome {
        case .success(let json):
            result.status = MigoHostServiceStatus(MIGO_HOST_SERVICE_STATUS_OK)
            status = json.withUnsafeBytes { bytes -> MigoResult in
                result.payload_json_utf8 = bytes.bindMemory(to: CChar.self).baseAddress
                result.payload_length = UInt32(bytes.count)
                return migo_session_complete_host_service_call(session, callId, &result)
            }
        case .failure(let message, let code):
            result.status = MigoHostServiceStatus(MIGO_HOST_SERVICE_STATUS_FAIL)
            if let code {
                result.flags = MigoHostServiceResultFlags(MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE)
                result.error_code = code
            }
            status = message.utf8CString.withUnsafeBufferPointer { text -> MigoResult in
                result.message_utf8 = text.baseAddress
                result.message_length = UInt32(text.count - 1)
                return migo_session_complete_host_service_call(session, callId, &result)
            }
        }
        if status != MIGO_OK {
            NSLog("MigoEngineSession: migo_session_complete_host_service_call returned \(status)")
        }
    }
}

/// The C callback: copy the call, then hand it to the provider on the main
/// thread, with a reply that completes through the session for as long as it
/// is open.
let migoOnHostServiceCall: MigoOnHostServiceCallFn = { userData, _, call in
    guard let call, let relay = callbackRelay(userData), let provider = relay.hostServices else { return }
    let record = call.pointee
    // The payload is the engine's only for the length of this call.
    let payload =
        record.payload_json_utf8.map { Data(bytes: $0, count: Int(record.payload_length)) } ?? Data()
    let callId = record.call_id
    let reply =
        callId == 0
        ? nil
        : MigoHostServiceReply { [weak owner = relay.owner] outcome in
            if Thread.isMainThread {
                owner?.completeHostServiceCall(callId, outcome)
            } else {
                DispatchQueue.main.async { owner?.completeHostServiceCall(callId, outcome) }
            }
        }
    let request = MigoHostServiceRequest(
        service: MigoHostServiceID(rawValue: record.service), method: record.method, payload: payload,
        reply: reply)
    if Thread.isMainThread {
        provider.handle(request)
    } else {
        DispatchQueue.main.async { provider.handle(request) }
    }
}
