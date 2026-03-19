export interface MediaAttachment {
  hash: string;
  ticket: string;
  mime_type: string;
  filename: string;
  size: number;
}

export interface Post {
  id: string;
  author: string;
  content: string;
  timestamp: number;
  media: MediaAttachment[];
  reply_to: string | null;
  reply_to_author: string | null;
  quote_of: string | null;
  quote_of_author: string | null;
  signature: string;
}

export interface PendingAttachment {
  hash: string;
  ticket: string;
  mime_type: string;
  filename: string;
  size: number;
  previewUrl: string;
}

export type Visibility = "public" | "listed" | "private";

export interface Profile {
  display_name: string;
  bio: string;
  avatar_hash: string | null;
  avatar_ticket: string | null;
  visibility: Visibility;
}

export interface SocialGraphEntry {
  pubkey: string;
  followed_at: number;
  first_seen: number;
  last_seen: number;
  is_online: boolean;
}

export interface RemoteSocialResult {
  follows: SocialGraphEntry[];
  hidden: boolean;
  cached: boolean;
}

export interface RemoteFollowersResult {
  followers: SocialGraphEntry[];
  hidden: boolean;
  cached: boolean;
}

export interface NodeStatus {
  node_id: string;
  has_relay: boolean;
  relay_url: string | null;
  follow_count: number;
  follower_count: number;
}

export interface Interaction {
  id: string;
  author: string;
  kind: "Like";
  target_post_id: string;
  target_author: string;
  timestamp: number;
  signature: string;
}

export interface PostCounts {
  likes: number;
  replies: number;
  reposts: number;
  liked_by_me: boolean;
  reposted_by_me: boolean;
}

export interface AppNotification {
  id: string;
  kind: "mention" | "like" | "reply" | "quote" | "follower";
  actor: string;
  target_post_id: string | null;
  post_id: string | null;
  timestamp: number;
  read: boolean;
}

export interface FollowRequestEntry {
  pubkey: string;
  timestamp: number;
  status: "pending" | "approved" | "denied";
  created_at: number;
  expires_at: number;
}

export interface SyncResult {
  posts: Post[];
  remote_total: number;
}

export interface SyncStatus {
  local_count: number;
}

export interface ConversationMeta {
  peer_pubkey: string;
  last_message_at: number;
  last_message_preview: string;
  unread_count: number;
}

export interface ServerEntry {
  url: string;
  name: string;
  description: string;
  node_id: string;
  registered_at: number | null;
  visibility: string;
  added_at: number;
  last_synced_at: number | null;
}

export interface ServerInfo {
  name: string;
  description: string;
  version: string;
  node_id: string;
  registered_users: number;
  total_posts: number;
  uptime_seconds: number;
  registration_open: boolean;
  retention_days: number;
}

export interface ServerFeedPost {
  id: string;
  author: string;
  content: string;
  timestamp: number;
  reply_to: string | null;
  media_hashes: string | null;
  signature: string | null;
}

export interface TrendingHashtag {
  tag: string;
  post_count: number;
  computed_at: number;
}

export interface ServerUser {
  pubkey: string;
  display_name: string | null;
  bio: string | null;
  avatar_hash: string | null;
  visibility: string;
  registered_at: number;
  post_count: number;
  latest_post_at: number | null;
  transport_node_id: string | null;
}

export interface UserSearchResponse {
  users: ServerUser[];
  total: number;
  query: string;
}

export interface ServerSearchPost {
  id: string;
  author: string;
  content: string;
  timestamp: number;
  media_json: string | null;
  reply_to: string | null;
  reply_to_author: string | null;
  quote_of: string | null;
  quote_of_author: string | null;
  signature: string;
  indexed_at: number;
}

export interface PostSearchResponse {
  posts: ServerSearchPost[];
  total: number;
  query: string;
}

export interface DeviceEntry {
  node_id: string;
  device_name: string;
  is_primary: boolean;
  added_at: number;
}

export interface LinkQrPayload {
  node_id: string;
  secret: string;
  relay_url: string | null;
}

export type CallState = "ringing" | "incoming" | "active" | "ended" | "failed";

export type StageRole = "Host" | "CoHost" | "Speaker" | "Listener";

export interface StageParticipant {
  pubkey: string;
  role: StageRole;
  display_name: string | null;
  avatar_hash: string | null;
  hand_raised: boolean;
  self_muted: boolean;
  host_muted: boolean;
}

export interface StageState {
  stage_id: string;
  title: string;
  host_pubkey: string;
  my_pubkey: string;
  my_role: StageRole;
  participants: StageParticipant[];
  started_at: number;
  ticket: string | null;
}

export type StageEvent =
  | ({ type: "state_snapshot" } & StageState)
  | { type: "participant_joined"; pubkey: string; role: StageRole }
  | { type: "participant_left"; pubkey: string }
  | { type: "role_changed"; pubkey: string; role: StageRole }
  | {
      type: "mute_changed";
      pubkey: string;
      self_muted: boolean;
      host_muted: boolean;
    }
  | { type: "hand_raised"; pubkey: string }
  | { type: "hand_lowered"; pubkey: string }
  | { type: "reaction"; pubkey: string; emoji: string }
  | { type: "chat"; pubkey: string; text: string }
  | { type: "ended"; stage_id: string }
  | { type: "kicked" }
  | { type: "auth_failed"; source: string; reason: string };

export interface StageAnnouncement {
  stage_id: string;
  title: string;
  ticket: string;
  host_pubkey: string;
  started_at: number;
}

export interface CallEvent {
  call_id: string;
  peer_pubkey: string;
  state: CallState;
}

export interface StoredMessage {
  id: string;
  conversation_id: string;
  from_pubkey: string;
  to_pubkey: string;
  content: string;
  timestamp: number;
  media: MediaAttachment[];
  read: boolean;
  delivered: boolean;
  reply_to: string | null;
}
