import Foundation

/// Asynchronous effects (spec 00 §4). `dispatch` returns an ack immediately; every outcome
/// arrives as an event carrying the same `commandId`.
///
/// Encoded as `{"type": "<camelCase tag>", …}` with the payload inlined. The encoding is
/// written out rather than derived so it stays reviewable against the Rust enum.
public enum CoreCommand: Sendable, Equatable {
    case createSession(
        groupId: String? = nil, title: String? = nil, workspaceRoot: String? = nil,
        modelRef: ModelRef? = nil)
    case sendPrompt(sessionId: String, text: String, attachmentIds: [String] = [])
    case abortRun(sessionId: String)
    case renameSession(sessionId: String, title: String)
    case deleteSession(sessionId: String)
    case archiveSession(sessionId: String, archived: Bool)
    case pinSession(sessionId: String, pinned: Bool)
    case moveSession(sessionId: String, groupId: String?, index: Int)
    case createGroup(name: String)
    case renameGroup(groupId: String, name: String)
    case deleteGroup(groupId: String)
    case reorderGroup(groupId: String, index: Int)
    case setGroupCollapsed(groupId: String, collapsed: Bool)
    case setSessionModel(sessionId: String, modelRef: ModelRef)
    case setWorkspaceRoot(sessionId: String, path: String?)
    /// The whole document, always — the core normalizes and echoes it back as
    /// `settings_changed` (spec 04 §2).
    case updateSettings(settings: Settings)
    case addAttachment(
        sessionId: String?, path: String? = nil, bytesBase64: String? = nil, filename: String,
        mime: String)
    case removeAttachment(attachmentId: String)
    case branchFromMessage(sessionId: String, entryId: String)
    case retryMessage(sessionId: String, entryId: String)

    public var type: String {
        switch self {
        case .createSession: "createSession"
        case .sendPrompt: "sendPrompt"
        case .abortRun: "abortRun"
        case .renameSession: "renameSession"
        case .deleteSession: "deleteSession"
        case .archiveSession: "archiveSession"
        case .pinSession: "pinSession"
        case .moveSession: "moveSession"
        case .createGroup: "createGroup"
        case .renameGroup: "renameGroup"
        case .deleteGroup: "deleteGroup"
        case .reorderGroup: "reorderGroup"
        case .setGroupCollapsed: "setGroupCollapsed"
        case .setSessionModel: "setSessionModel"
        case .setWorkspaceRoot: "setWorkspaceRoot"
        case .updateSettings: "updateSettings"
        case .addAttachment: "addAttachment"
        case .removeAttachment: "removeAttachment"
        case .branchFromMessage: "branchFromMessage"
        case .retryMessage: "retryMessage"
        }
    }

    /// The session this command acts on, where there is one — the stores use it to decide
    /// whether an optimistic local edit belongs to them.
    public var sessionId: String? {
        switch self {
        case let .sendPrompt(id, _, _), let .abortRun(id), let .renameSession(id, _),
            let .deleteSession(id), let .archiveSession(id, _), let .pinSession(id, _),
            let .moveSession(id, _, _), let .setSessionModel(id, _),
            let .setWorkspaceRoot(id, _), let .branchFromMessage(id, _),
            let .retryMessage(id, _):
            id
        case let .addAttachment(id, _, _, _, _):
            id
        default:
            nil
        }
    }
}

extension CoreCommand: Codable {
    private enum CodingKeys: String, CodingKey {
        case type, groupId, title, workspaceRoot, modelRef, sessionId, text, attachmentIds
        case archived, pinned, index, name, collapsed, path, settings, bytesBase64, filename
        case mime, attachmentId, entryId
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let type = try c.decode(String.self, forKey: .type)

        func string(_ key: CodingKeys) throws -> String { try c.decode(String.self, forKey: key) }
        func optional(_ key: CodingKeys) throws -> String? {
            try c.decodeIfPresent(String.self, forKey: key)
        }
        func bool(_ key: CodingKeys) throws -> Bool { try c.decode(Bool.self, forKey: key) }

        switch type {
        case "createSession":
            self = .createSession(
                groupId: try optional(.groupId),
                title: try optional(.title),
                workspaceRoot: try optional(.workspaceRoot),
                modelRef: try c.decodeIfPresent(ModelRef.self, forKey: .modelRef)
            )
        case "sendPrompt":
            self = .sendPrompt(
                sessionId: try string(.sessionId),
                text: try string(.text),
                attachmentIds: try c.decodeIfPresent([String].self, forKey: .attachmentIds) ?? []
            )
        case "abortRun":
            self = .abortRun(sessionId: try string(.sessionId))
        case "renameSession":
            self = .renameSession(sessionId: try string(.sessionId), title: try string(.title))
        case "deleteSession":
            self = .deleteSession(sessionId: try string(.sessionId))
        case "archiveSession":
            self = .archiveSession(sessionId: try string(.sessionId), archived: try bool(.archived))
        case "pinSession":
            self = .pinSession(sessionId: try string(.sessionId), pinned: try bool(.pinned))
        case "moveSession":
            self = .moveSession(
                sessionId: try string(.sessionId),
                groupId: try optional(.groupId),
                index: try c.decode(Int.self, forKey: .index)
            )
        case "createGroup":
            self = .createGroup(name: try string(.name))
        case "renameGroup":
            self = .renameGroup(groupId: try string(.groupId), name: try string(.name))
        case "deleteGroup":
            self = .deleteGroup(groupId: try string(.groupId))
        case "reorderGroup":
            self = .reorderGroup(
                groupId: try string(.groupId), index: try c.decode(Int.self, forKey: .index))
        case "setGroupCollapsed":
            self = .setGroupCollapsed(
                groupId: try string(.groupId), collapsed: try bool(.collapsed))
        case "setSessionModel":
            self = .setSessionModel(
                sessionId: try string(.sessionId),
                modelRef: try c.decode(ModelRef.self, forKey: .modelRef)
            )
        case "setWorkspaceRoot":
            self = .setWorkspaceRoot(sessionId: try string(.sessionId), path: try optional(.path))
        case "updateSettings":
            self = .updateSettings(settings: try c.decode(Settings.self, forKey: .settings))
        case "addAttachment":
            self = .addAttachment(
                sessionId: try optional(.sessionId),
                path: try optional(.path),
                bytesBase64: try optional(.bytesBase64),
                filename: try string(.filename),
                mime: try string(.mime)
            )
        case "removeAttachment":
            self = .removeAttachment(attachmentId: try string(.attachmentId))
        case "branchFromMessage":
            self = .branchFromMessage(
                sessionId: try string(.sessionId), entryId: try string(.entryId))
        case "retryMessage":
            self = .retryMessage(sessionId: try string(.sessionId), entryId: try string(.entryId))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .type, in: c,
                debugDescription:
                    "unknown command '\(type)' — the Swift mirror is behind the core"
            )
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(type, forKey: .type)
        switch self {
        case let .createSession(groupId, title, workspaceRoot, modelRef):
            try c.encodeIfPresent(groupId, forKey: .groupId)
            try c.encodeIfPresent(title, forKey: .title)
            try c.encodeIfPresent(workspaceRoot, forKey: .workspaceRoot)
            try c.encodeIfPresent(modelRef, forKey: .modelRef)
        case let .sendPrompt(sessionId, text, attachmentIds):
            try c.encode(sessionId, forKey: .sessionId)
            try c.encode(text, forKey: .text)
            try c.encode(attachmentIds, forKey: .attachmentIds)
        case let .abortRun(sessionId), let .deleteSession(sessionId):
            try c.encode(sessionId, forKey: .sessionId)
        case let .renameSession(sessionId, title):
            try c.encode(sessionId, forKey: .sessionId)
            try c.encode(title, forKey: .title)
        case let .archiveSession(sessionId, archived):
            try c.encode(sessionId, forKey: .sessionId)
            try c.encode(archived, forKey: .archived)
        case let .pinSession(sessionId, pinned):
            try c.encode(sessionId, forKey: .sessionId)
            try c.encode(pinned, forKey: .pinned)
        case let .moveSession(sessionId, groupId, index):
            try c.encode(sessionId, forKey: .sessionId)
            try c.encodeIfPresent(groupId, forKey: .groupId)
            try c.encode(index, forKey: .index)
        case let .createGroup(name):
            try c.encode(name, forKey: .name)
        case let .renameGroup(groupId, name):
            try c.encode(groupId, forKey: .groupId)
            try c.encode(name, forKey: .name)
        case let .deleteGroup(groupId):
            try c.encode(groupId, forKey: .groupId)
        case let .reorderGroup(groupId, index):
            try c.encode(groupId, forKey: .groupId)
            try c.encode(index, forKey: .index)
        case let .setGroupCollapsed(groupId, collapsed):
            try c.encode(groupId, forKey: .groupId)
            try c.encode(collapsed, forKey: .collapsed)
        case let .setSessionModel(sessionId, modelRef):
            try c.encode(sessionId, forKey: .sessionId)
            try c.encode(modelRef, forKey: .modelRef)
        case let .setWorkspaceRoot(sessionId, path):
            try c.encode(sessionId, forKey: .sessionId)
            try c.encodeIfPresent(path, forKey: .path)
        case let .updateSettings(settings):
            try c.encode(settings, forKey: .settings)
        case let .addAttachment(sessionId, path, bytesBase64, filename, mime):
            try c.encodeIfPresent(sessionId, forKey: .sessionId)
            try c.encodeIfPresent(path, forKey: .path)
            try c.encodeIfPresent(bytesBase64, forKey: .bytesBase64)
            try c.encode(filename, forKey: .filename)
            try c.encode(mime, forKey: .mime)
        case let .removeAttachment(attachmentId):
            try c.encode(attachmentId, forKey: .attachmentId)
        case let .branchFromMessage(sessionId, entryId), let .retryMessage(sessionId, entryId):
            try c.encode(sessionId, forKey: .sessionId)
            try c.encode(entryId, forKey: .entryId)
        }
    }
}

/// The immediate reply to `dispatch`. Everything else arrives as an event.
public struct CommandAck: Codable, Sendable, Equatable {
    public var commandId: String

    public init(commandId: String) { self.commandId = commandId }
}

public typealias CommandID = String
