import Foundation

/// A model as the picker shows it: the model, the provider it belongs to, and a match score
/// when it came from a search (F8.3).
public struct ModelHit: Sendable, Equatable, Identifiable {
    public var provider: Provider
    public var model: Model
    public var score: Double

    public var id: String { "\(provider.id)/\(model.id)" }
    public var ref: ModelRef {
        ModelRef(
            providerId: provider.id, modelId: model.id,
            thinkingLevel: model.thinkingLevels.contains(.high) ? .high : (model.thinkingLevels.first ?? .off))
    }
}

/// Providers and models, loaded once (spec 07 §4). Static data — nothing invalidates it.
@MainActor
@Observable
public final class CatalogStore {
    public private(set) var catalog = Catalog()
    public private(set) var isLoaded = false

    @ObservationIgnored private let client: CoreClient

    public init(client: CoreClient) {
        self.client = client
    }

    public init(client: CoreClient, catalog: Catalog) {
        self.client = client
        self.catalog = catalog
        isLoaded = true
    }

    /// Preview seeding — synchronous.
    func seed(_ catalog: Catalog) {
        self.catalog = catalog
        isLoaded = true
    }

    public func load() async {
        guard !isLoaded else { return }
        do {
            catalog = try await client.query(GetCatalog())
            isLoaded = true
        } catch {
            Log.stores.error("getCatalog failed: \(String(describing: error), privacy: .public)")
        }
    }

    public var providers: [Provider] { catalog.providers }

    public func provider(id: String) -> Provider? { catalog.provider(id: id) }
    public func model(_ ref: ModelRef) -> Model? { catalog.model(ref) }
    public func displayName(_ ref: ModelRef) -> String { catalog.displayName(ref) }

    /// What the effort picker may offer for a model (F8.2). A model that cannot reason
    /// offers `off` only.
    public func thinkingLevels(for ref: ModelRef) -> [ThinkingLevel] {
        let levels = model(ref)?.thinkingLevels ?? []
        return levels.isEmpty ? [.off] : levels
    }

    /// Substring match, exact-prefix first — enough for a popover over a few dozen models.
    public func search(_ query: String) -> [ModelHit] {
        let all = catalog.allModels.map { ModelHit(provider: $0.provider, model: $0.model, score: 0) }
        let q = query.trimmingCharacters(in: .whitespaces).lowercased()
        guard !q.isEmpty else { return all }

        return
            all
            .compactMap { hit -> ModelHit? in
                let name = hit.model.name.lowercased()
                let id = hit.model.id.lowercased()
                let provider = hit.provider.name.lowercased()
                let score: Double =
                    if name.hasPrefix(q) || id.hasPrefix(q) {
                        3
                    } else if name.contains(q) || id.contains(q) {
                        2
                    } else if provider.contains(q) {
                        1
                    } else {
                        0
                    }
                guard score > 0 else { return nil }
                var scored = hit
                scored.score = score
                return scored
            }
            .sorted { a, b in
                a.score == b.score ? a.model.name < b.model.name : a.score > b.score
            }
    }
}
