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

export interface FollowEntry {
  pubkey: string;
  alias: string | null;
  followed_at: number;
}

export interface FollowerEntry {
  pubkey: string;
  first_seen: number;
  last_seen: number;
  is_online: boolean;
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
