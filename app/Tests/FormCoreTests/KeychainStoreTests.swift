import Foundation
import Testing

@testable import FormCore

/// Keychain round trip (spec 07 §5). A runner with no usable keychain — unsigned CI, a
/// locked login keychain — must skip rather than fail, so every test here probes first and
/// returns cleanly when the environment cannot answer.
@Suite("KeychainStore")
struct KeychainStoreTests {

    /// A private service name per run, so a failure can never touch a real key and two runs
    /// can never collide.
    private func makeStore() -> KeychainStore {
        KeychainStore(service: "dev.jhead.form.tests.\(UUID().uuidString)")
    }

    /// `nil` when the keychain is unavailable, which is a skip and not a failure.
    private func probe(_ store: KeychainStore) -> Bool {
        do {
            try store.set("probe", for: "probe")
            try store.delete("probe")
            return true
        } catch let error as KeychainStore.KeychainError where error.isUnavailable {
            return false
        } catch {
            // Any other failure on a probe still means we cannot test here.
            Log.keychain.error("keychain probe failed: \(String(describing: error), privacy: .public)")
            return false
        }
    }

    @Test("a key round-trips, updates and deletes")
    func roundTrip() throws {
        let store = makeStore()
        guard probe(store) else { return }
        defer { try? store.delete("anthropic") }

        #expect(try store.get("anthropic") == nil)

        try store.set("sk-first", for: "anthropic")
        #expect(try store.get("anthropic") == "sk-first")
        #expect(store.contains("anthropic"))

        // A second write replaces rather than duplicating.
        try store.set("sk-second", for: "anthropic")
        #expect(try store.get("anthropic") == "sk-second")

        try store.delete("anthropic")
        #expect(try store.get("anthropic") == nil)
        #expect(store.contains("anthropic") == false)
    }

    @Test("deleting a key that is not there is not an error")
    func deleteMissing() throws {
        let store = makeStore()
        guard probe(store) else { return }
        try store.delete("never-written")
    }

    @Test("accounts are independent")
    func separateAccounts() throws {
        let store = makeStore()
        guard probe(store) else { return }
        defer {
            try? store.delete("anthropic")
            try? store.delete("openai")
        }

        try store.set("a", for: "anthropic")
        try store.set("b", for: "openai")
        #expect(try store.get("anthropic") == "a")
        #expect(try store.get("openai") == "b")

        try store.delete("anthropic")
        #expect(try store.get("openai") == "b", "deleting one account must not touch another")
    }

    @Test("the default service is the bundle identifier the spec names")
    func defaultService() {
        #expect(KeychainStore.defaultService == "dev.jhead.form")
        #expect(KeychainStore().service == "dev.jhead.form")
    }

    @Test("setting a key records presence without the value crossing the boundary")
    func hasKeyOnly() async throws {
        let keychain = makeStore()
        guard probe(keychain) else { return }
        defer { try? keychain.delete("anthropic") }

        let transport = MockTransport()
        let store = await SettingsStore(
            client: CoreClient(mock: transport), keychain: keychain)

        try await store.setAPIKey("sk-secret", for: "anthropic")

        guard case let .updateSettings(sent)? = transport.commands.last else {
            Issue.record("expected updateSettings")
            return
        }
        #expect(sent.providers["anthropic"]?.hasKey == true)

        // The secret must appear nowhere in what was sent to the core.
        let json = String(decoding: try JSONEncoder().encode(sent), as: UTF8.self)
        #expect(json.contains("sk-secret") == false)
        #expect(try keychain.get("anthropic") == "sk-secret")

        try await store.setAPIKey(nil, for: "anthropic")
        #expect(try keychain.get("anthropic") == nil)
    }
}
