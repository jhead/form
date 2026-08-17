import Foundation
import Security

/// Provider API keys, in the macOS Keychain (spec 07 §5, F8.5).
///
/// Service is `dev.jhead.form`, account is the provider id. **Values are never logged** —
/// the log records the operation, the account and the status code, never the secret. Keys
/// never cross the FFI boundary; the core only ever learns `hasKey`.
public struct KeychainStore: Sendable {
    public static let defaultService = "dev.jhead.form"

    public let service: String

    public init(service: String = KeychainStore.defaultService) {
        self.service = service
    }

    public enum KeychainError: Error, Equatable, CustomStringConvertible {
        case unhandled(status: OSStatus)
        case unexpectedData
        /// No keychain is available — an unsigned CI runner, typically.
        case unavailable(status: OSStatus)

        public var description: String {
            switch self {
            case let .unhandled(status): "keychain error \(status)"
            case .unexpectedData: "keychain item was not UTF-8 text"
            case let .unavailable(status): "keychain unavailable (\(status))"
            }
        }

        /// `true` when the failure is the environment, not the call — the caller should skip
        /// rather than fail.
        public var isUnavailable: Bool {
            if case .unavailable = self { return true }
            return false
        }
    }

    private func baseQuery(_ account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    /// Writes or replaces the key for `account`.
    public func set(_ value: String, for account: String) throws {
        let data = Data(value.utf8)
        var query = baseQuery(account)

        let update: [String: Any] = [kSecValueData as String: data]
        var status = SecItemUpdate(query as CFDictionary, update as CFDictionary)

        if status == errSecItemNotFound {
            query[kSecValueData as String] = data
            query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
            status = SecItemAdd(query as CFDictionary, nil)
        }

        guard status == errSecSuccess else {
            Log.keychain.error("set failed for \(account, privacy: .public): \(status)")
            throw Self.error(for: status)
        }
    }

    /// The key for `account`, or `nil` when there is none.
    public func get(_ account: String) throws -> String? {
        var query = baseQuery(account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne

        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        switch status {
        case errSecSuccess:
            guard let data = item as? Data, let value = String(data: data, encoding: .utf8) else {
                throw KeychainError.unexpectedData
            }
            return value
        case errSecItemNotFound:
            return nil
        default:
            Log.keychain.error("get failed for \(account, privacy: .public): \(status)")
            throw Self.error(for: status)
        }
    }

    /// Removing a key that is not there is not an error — deleting twice is a normal thing
    /// for a preferences pane to do.
    public func delete(_ account: String) throws {
        let status = SecItemDelete(baseQuery(account) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            Log.keychain.error("delete failed for \(account, privacy: .public): \(status)")
            throw Self.error(for: status)
        }
    }

    public func contains(_ account: String) -> Bool {
        ((try? get(account)) ?? nil) != nil
    }

    private static func error(for status: OSStatus) -> KeychainError {
        switch status {
        // -34018 (errSecMissingEntitlement) and errSecNotAvailable are what an unsigned or
        // headless process gets; they say nothing about the caller's arguments.
        case -34018, errSecNotAvailable, errSecInteractionNotAllowed:
            .unavailable(status: status)
        default:
            .unhandled(status: status)
        }
    }
}
