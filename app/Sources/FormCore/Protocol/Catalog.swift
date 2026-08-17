import Foundation

/// Providers and models (spec 04 §1). Static data from the core, loaded once — pricing and
/// context windows here feed both the model picker (F8.3) and the dashboard's cost figures.
public struct Catalog: Codable, Sendable, Equatable {
    public var providers: [Provider]

    public init(providers: [Provider] = []) { self.providers = providers }

    public func provider(id: String) -> Provider? { providers.first { $0.id == id } }

    public func model(_ ref: ModelRef) -> Model? {
        provider(id: ref.providerId)?.models.first { $0.id == ref.modelId }
    }

    /// Display form for the composer chip: `Opus 5`, falling back to the raw id.
    public func displayName(_ ref: ModelRef) -> String {
        model(ref)?.name ?? ref.modelId
    }

    public var allModels: [(provider: Provider, model: Model)] {
        providers.flatMap { p in p.models.map { (p, $0) } }
    }
}

public struct Provider: Codable, Sendable, Equatable, Identifiable {
    public var id: String
    public var name: String
    public var baseUrl: String
    public var auth: [AuthMethod]
    public var envVars: [String]
    public var models: [Model]

    public init(
        id: String, name: String, baseUrl: String, auth: [AuthMethod] = [],
        envVars: [String] = [], models: [Model] = []
    ) {
        self.id = id
        self.name = name
        self.baseUrl = baseUrl
        self.auth = auth
        self.envVars = envVars
        self.models = models
    }

    public var needsApiKey: Bool { auth.contains(.apiKey) }
}

public struct Pricing: Codable, Sendable, Equatable {
    /// USD per 1M tokens.
    public var input: Double
    public var output: Double
    public var cacheRead: Double
    public var cacheWrite: Double

    public init(
        input: Double = 0, output: Double = 0, cacheRead: Double = 0, cacheWrite: Double = 0
    ) {
        self.input = input
        self.output = output
        self.cacheRead = cacheRead
        self.cacheWrite = cacheWrite
    }
}

public struct Capabilities: Codable, Sendable, Equatable {
    public var vision: Bool
    public var tools: Bool
    public var reasoning: Bool
    public var caching: Bool
    public var streaming: Bool

    public init(
        vision: Bool = false, tools: Bool = false, reasoning: Bool = false,
        caching: Bool = false, streaming: Bool = false
    ) {
        self.vision = vision
        self.tools = tools
        self.reasoning = reasoning
        self.caching = caching
        self.streaming = streaming
    }
}

public struct Model: Codable, Sendable, Equatable, Identifiable {
    public var id: String
    public var name: String
    public var family: String
    public var contextWindow: Int64
    public var maxOutput: Int64
    public var pricing: Pricing
    public var capabilities: Capabilities
    /// What the effort picker may offer for this model (F8.2).
    public var thinkingLevels: [ThinkingLevel]
    public var released: String?
    public var deprecated: Bool

    public init(
        id: String, name: String, family: String, contextWindow: Int64, maxOutput: Int64,
        pricing: Pricing = Pricing(), capabilities: Capabilities = Capabilities(),
        thinkingLevels: [ThinkingLevel] = [.off], released: String? = nil,
        deprecated: Bool = false
    ) {
        self.id = id
        self.name = name
        self.family = family
        self.contextWindow = contextWindow
        self.maxOutput = maxOutput
        self.pricing = pricing
        self.capabilities = capabilities
        self.thinkingLevels = thinkingLevels
        self.released = released
        self.deprecated = deprecated
    }

    private enum CodingKeys: String, CodingKey {
        case id, name, family, contextWindow, maxOutput, pricing, capabilities
        case thinkingLevels, released, deprecated
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        family = try c.decode(String.self, forKey: .family)
        contextWindow = try c.decode(Int64.self, forKey: .contextWindow)
        maxOutput = try c.decode(Int64.self, forKey: .maxOutput)
        pricing = try c.decode(Pricing.self, forKey: .pricing)
        capabilities = try c.decode(Capabilities.self, forKey: .capabilities)
        thinkingLevels = try c.decodeIfPresent([ThinkingLevel].self, forKey: .thinkingLevels) ?? []
        released = try c.decodeIfPresent(String.self, forKey: .released)
        deprecated = try c.decodeIfPresent(Bool.self, forKey: .deprecated) ?? false
    }
}
