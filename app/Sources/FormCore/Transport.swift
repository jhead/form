import Foundation
import FormFFI

/// The seam that keeps the sidecar option open (PRD §4.1). Nothing above `CoreClient` may
/// reference `FormFFI`; everything goes through this protocol, so a `SubprocessTransport`
/// speaking the same JSON over a pipe is an additive change.
public protocol CoreTransport: AnyObject, Sendable {
    func query(_ json: String) throws -> String
    func dispatch(_ json: String) throws -> String
    /// Returns a token for `unsubscribe`. The handler is invoked on a background thread, in
    /// order, never concurrently.
    func subscribe(_ handler: @escaping @Sendable (String) -> Void) throws -> Int32
    func unsubscribe(_ token: Int32)
    func shutdown()
}

public enum TransportError: Error, CustomStringConvertible {
    case abiMismatch(expected: UInt32, actual: UInt32)
    case startupFailed(String)
    case invalidHandle
    case encodingFailed

    public var description: String {
        switch self {
        case let .abiMismatch(expected, actual):
            "core ABI v\(actual) does not match the client's v\(expected); rebuild the Rust core"
        case let .startupFailed(message): "core failed to start: \(message)"
        case .invalidHandle: "core handle is no longer valid"
        case .encodingFailed: "payload was not valid UTF-8"
        }
    }
}

/// Holds the Swift closure a C callback needs to reach. Passed to Rust as an opaque `void*`
/// and handed back verbatim; Rust never dereferences it.
private final class CallbackBox: @unchecked Sendable {
    let handler: @Sendable (String) -> Void
    init(_ handler: @escaping @Sendable (String) -> Void) { self.handler = handler }
}

/// The C callback. It does nothing but forward — no allocation-heavy work, no re-entry into
/// the core (spec 00 §7).
private func formEventTrampoline(json: UnsafePointer<CChar>?, len: Int, ctx: UnsafeMutableRawPointer?) {
    guard let json, let ctx else { return }
    let box = Unmanaged<CallbackBox>.fromOpaque(ctx).takeUnretainedValue()
    box.handler(String(cString: json))
}

public final class FFITransport: CoreTransport, @unchecked Sendable {
    private let handle: OpaquePointer
    private let lock = NSLock()
    private var boxes: [Int32: CallbackBox] = [:]
    private var isShutDown = false

    public init(config: CoreConfig) throws {
        let expected = UInt32(FORM_ABI_VERSION)
        let actual = form_abi_version()
        guard actual == expected else {
            throw TransportError.abiMismatch(expected: expected, actual: actual)
        }

        let encoder = JSONEncoder()
        let data = try encoder.encode(config)
        guard let json = String(data: data, encoding: .utf8) else {
            throw TransportError.encodingFailed
        }

        // `FormCoreHandle` is an incomplete C type, so it arrives as an OpaquePointer.
        guard let handle = json.withCString({ form_core_new($0) }) else {
            let message = form_last_error().map { String(cString: $0) } ?? "unknown"
            throw TransportError.startupFailed(message)
        }
        self.handle = handle
    }

    deinit {
        shutdown()
    }

    public func query(_ json: String) throws -> String {
        try call(json) { form_core_query(self.handle, $0) }
    }

    public func dispatch(_ json: String) throws -> String {
        try call(json) { form_core_dispatch(self.handle, $0) }
    }

    private func call(
        _ json: String,
        _ body: (UnsafePointer<CChar>) -> UnsafeMutablePointer<CChar>?
    ) throws -> String {
        try json.withCString { input in
            guard let raw = body(input) else { throw TransportError.invalidHandle }
            defer { form_string_free(raw) }
            return String(cString: raw)
        }
    }

    public func subscribe(_ handler: @escaping @Sendable (String) -> Void) throws -> Int32 {
        let box = CallbackBox(handler)
        // Unretained on the C side; `boxes` is what keeps it alive until unsubscribe.
        let ctx = Unmanaged.passUnretained(box).toOpaque()
        let token = form_core_subscribe(handle, formEventTrampoline, ctx)
        guard token > 0 else { throw TransportError.invalidHandle }
        lock.withLock { boxes[token] = box }
        return token
    }

    public func unsubscribe(_ token: Int32) {
        // Rust guarantees no further invocation once this returns, so releasing the box
        // afterwards cannot race a delivery.
        form_core_unsubscribe(handle, token)
        lock.withLock { _ = boxes.removeValue(forKey: token) }
    }

    public func shutdown() {
        let shouldFree: Bool = lock.withLock {
            guard !isShutDown else { return false }
            isShutDown = true
            return true
        }
        guard shouldFree else { return }
        for token in lock.withLock({ Array(boxes.keys) }) {
            form_core_unsubscribe(handle, token)
        }
        lock.withLock { boxes.removeAll() }
        form_core_free(handle)
    }
}
