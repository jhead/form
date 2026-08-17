import Foundation

/// The block tree the core produces (spec 05). Parsing and syntax highlighting run in Rust;
/// what crosses the boundary is structure and **scope names, never colors** — `FormDesign`
/// maps scopes onto the active theme (PRD §4.4).
public struct MarkdownDoc: Codable, Sendable, Equatable {
    public var blocks: [MarkdownBlock]

    public init(blocks: [MarkdownBlock] = []) { self.blocks = blocks }
}

public struct MarkdownBlock: Codable, Sendable, Equatable, Identifiable {
    /// Stable across incremental re-parses, so SwiftUI keeps view identity while streaming.
    public var id: String
    /// Flattened into this object on the wire.
    public var kind: BlockKind

    public init(id: String, kind: BlockKind) {
        self.id = id
        self.kind = kind
    }

    private enum CodingKeys: String, CodingKey { case id }

    public init(from decoder: Decoder) throws {
        id = try decoder.container(keyedBy: CodingKeys.self).decode(String.self, forKey: .id)
        kind = try BlockKind(from: decoder)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id, forKey: .id)
        try kind.encode(to: encoder)
    }
}

public enum ColumnAlign: String, Codable, Sendable, Equatable {
    case left, center, right, none
}

/// Inline content. Tagged on `type`, `camelCase`.
public indirect enum Span: Sendable, Equatable {
    case text(text: String)
    case emphasis(spans: [Span])
    case strong(spans: [Span])
    case strike(spans: [Span])
    case code(text: String)
    case link(url: String, title: String?, spans: [Span])
    case footnoteRef(label: String)
    case `break`(hard: Bool)
    case unknown(type: String, raw: JSONValue)

    public var type: String {
        switch self {
        case .text: "text"
        case .emphasis: "emphasis"
        case .strong: "strong"
        case .strike: "strike"
        case .code: "code"
        case .link: "link"
        case .footnoteRef: "footnoteRef"
        case .break: "break"
        case let .unknown(type, _): type
        }
    }

    /// The text this span and its children contribute, for selection and copy (F7.4).
    public var plainText: String {
        switch self {
        case let .text(text), let .code(text): text
        case let .emphasis(spans), let .strong(spans), let .strike(spans): spans.map(\.plainText)
            .joined()
        case let .link(_, _, spans): spans.map(\.plainText).joined()
        case .footnoteRef, .break, .unknown: ""
        }
    }
}

extension Span: Codable {
    private enum CodingKeys: String, CodingKey { case type, text, spans, url, title, label, hard }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let type = try c.decode(String.self, forKey: .type)
        func spans() throws -> [Span] {
            try c.decodeIfPresent([Span].self, forKey: .spans) ?? []
        }
        switch type {
        case "text": self = .text(text: try c.decode(String.self, forKey: .text))
        case "emphasis": self = .emphasis(spans: try spans())
        case "strong": self = .strong(spans: try spans())
        case "strike": self = .strike(spans: try spans())
        case "code": self = .code(text: try c.decode(String.self, forKey: .text))
        case "link":
            self = .link(
                url: try c.decode(String.self, forKey: .url),
                title: try c.decodeIfPresent(String.self, forKey: .title),
                spans: try spans()
            )
        case "footnoteRef": self = .footnoteRef(label: try c.decode(String.self, forKey: .label))
        case "break": self = .break(hard: try c.decodeIfPresent(Bool.self, forKey: .hard) ?? false)
        default: self = .unknown(type: type, raw: try JSONValue(from: decoder))
        }
    }

    public func encode(to encoder: Encoder) throws {
        if case let .unknown(_, raw) = self {
            try raw.encode(to: encoder)
            return
        }
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(type, forKey: .type)
        switch self {
        case let .text(text), let .code(text):
            try c.encode(text, forKey: .text)
        case let .emphasis(spans), let .strong(spans), let .strike(spans):
            try c.encode(spans, forKey: .spans)
        case let .link(url, title, spans):
            try c.encode(url, forKey: .url)
            try c.encodeIfPresent(title, forKey: .title)
            try c.encode(spans, forKey: .spans)
        case let .footnoteRef(label):
            try c.encode(label, forKey: .label)
        case let .break(hard):
            try c.encode(hard, forKey: .hard)
        case .unknown:
            break
        }
    }
}

public struct ListItem: Codable, Sendable, Equatable {
    /// `nil` for a plain bullet, non-nil for a task list.
    public var checked: Bool?
    public var blocks: [MarkdownBlock]

    public init(checked: Bool? = nil, blocks: [MarkdownBlock] = []) {
        self.checked = checked
        self.blocks = blocks
    }
}

/// A highlighted range in a code block. Offsets are **UTF-16 code units** so they apply to
/// an `AttributedString` without re-encoding.
public struct CodeToken: Codable, Sendable, Equatable {
    public var start: Int
    public var len: Int
    /// A `syntect` scope name, e.g. `keyword.control.rust`. Never a color.
    public var scope: String

    public init(start: Int, len: Int, scope: String) {
        self.start = start
        self.len = len
        self.scope = scope
    }
}

public enum BlockKind: Sendable, Equatable {
    case paragraph(spans: [Span])
    case heading(level: Int, spans: [Span], anchor: String)
    case codeBlock(language: String?, code: String, tokens: [CodeToken], partial: Bool)
    case list(ordered: Bool, start: Int64, tight: Bool, items: [ListItem])
    case quote(blocks: [MarkdownBlock])
    case table(align: [ColumnAlign], header: [[Span]], rows: [[[Span]]])
    case rule
    case image(url: String, alt: String, title: String?)
    /// Captured, never interpreted — rendered as escaped text.
    case html(raw: String)
    case footnoteDef(label: String, blocks: [MarkdownBlock])
    case unknown(type: String, raw: JSONValue)

    public var type: String {
        switch self {
        case .paragraph: "paragraph"
        case .heading: "heading"
        case .codeBlock: "codeBlock"
        case .list: "list"
        case .quote: "quote"
        case .table: "table"
        case .rule: "rule"
        case .image: "image"
        case .html: "html"
        case .footnoteDef: "footnoteDef"
        case let .unknown(type, _): type
        }
    }
}

extension BlockKind: Codable {
    private enum CodingKeys: String, CodingKey {
        case type, spans, level, anchor, language, code, tokens, partial, ordered, start
        case tight, items, blocks, align, header, rows, url, alt, title, raw, label
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let type = try c.decode(String.self, forKey: .type)
        func spans() throws -> [Span] { try c.decodeIfPresent([Span].self, forKey: .spans) ?? [] }
        func blocks() throws -> [MarkdownBlock] {
            try c.decodeIfPresent([MarkdownBlock].self, forKey: .blocks) ?? []
        }
        switch type {
        case "paragraph":
            self = .paragraph(spans: try spans())
        case "heading":
            self = .heading(
                level: try c.decode(Int.self, forKey: .level),
                spans: try spans(),
                anchor: try c.decodeIfPresent(String.self, forKey: .anchor) ?? ""
            )
        case "codeBlock":
            self = .codeBlock(
                language: try c.decodeIfPresent(String.self, forKey: .language),
                code: try c.decode(String.self, forKey: .code),
                tokens: try c.decodeIfPresent([CodeToken].self, forKey: .tokens) ?? [],
                partial: try c.decodeIfPresent(Bool.self, forKey: .partial) ?? false
            )
        case "list":
            self = .list(
                ordered: try c.decodeIfPresent(Bool.self, forKey: .ordered) ?? false,
                start: try c.decodeIfPresent(Int64.self, forKey: .start) ?? 1,
                tight: try c.decodeIfPresent(Bool.self, forKey: .tight) ?? true,
                items: try c.decodeIfPresent([ListItem].self, forKey: .items) ?? []
            )
        case "quote":
            self = .quote(blocks: try blocks())
        case "table":
            self = .table(
                align: try c.decodeIfPresent([ColumnAlign].self, forKey: .align) ?? [],
                header: try c.decodeIfPresent([[Span]].self, forKey: .header) ?? [],
                rows: try c.decodeIfPresent([[[Span]]].self, forKey: .rows) ?? []
            )
        case "rule":
            self = .rule
        case "image":
            self = .image(
                url: try c.decode(String.self, forKey: .url),
                alt: try c.decodeIfPresent(String.self, forKey: .alt) ?? "",
                title: try c.decodeIfPresent(String.self, forKey: .title)
            )
        case "html":
            self = .html(raw: try c.decode(String.self, forKey: .raw))
        case "footnoteDef":
            self = .footnoteDef(
                label: try c.decode(String.self, forKey: .label), blocks: try blocks())
        default:
            self = .unknown(type: type, raw: try JSONValue(from: decoder))
        }
    }

    public func encode(to encoder: Encoder) throws {
        if case let .unknown(_, raw) = self {
            try raw.encode(to: encoder)
            return
        }
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(type, forKey: .type)
        switch self {
        case let .paragraph(spans):
            try c.encode(spans, forKey: .spans)
        case let .heading(level, spans, anchor):
            try c.encode(level, forKey: .level)
            try c.encode(spans, forKey: .spans)
            try c.encode(anchor, forKey: .anchor)
        case let .codeBlock(language, code, tokens, partial):
            try c.encodeIfPresent(language, forKey: .language)
            try c.encode(code, forKey: .code)
            try c.encode(tokens, forKey: .tokens)
            try c.encode(partial, forKey: .partial)
        case let .list(ordered, start, tight, items):
            try c.encode(ordered, forKey: .ordered)
            try c.encode(start, forKey: .start)
            try c.encode(tight, forKey: .tight)
            try c.encode(items, forKey: .items)
        case let .quote(blocks), let .footnoteDef(_, blocks):
            if case let .footnoteDef(label, _) = self { try c.encode(label, forKey: .label) }
            try c.encode(blocks, forKey: .blocks)
        case let .table(align, header, rows):
            try c.encode(align, forKey: .align)
            try c.encode(header, forKey: .header)
            try c.encode(rows, forKey: .rows)
        case .rule:
            break
        case let .image(url, alt, title):
            try c.encode(url, forKey: .url)
            try c.encode(alt, forKey: .alt)
            try c.encodeIfPresent(title, forKey: .title)
        case let .html(raw):
            try c.encode(raw, forKey: .raw)
        case .unknown:
            break
        }
    }
}
