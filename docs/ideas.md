# Ideas - Everything We Could Build

A brain dump of every possible feature, improvement, and direction for Proscenium.

---

## Social Features

- ~~**Likes, replies, reposts**~~ -- DONE: full implementation with real-time counts, signature verification
- **Hashtags** -- parse #tags from post content, make clickable, local search by tag
- **Mentions** -- @pubkey or @alias inline in posts, notify mentioned user
- ~~**Bookmarks**~~ -- DONE: local-only saved posts, private (never broadcast)
- **Polls** -- structured poll data in posts, votes as interactions
- **Quote posts** -- repost with commentary (embed original post + your text above)
- **Post editing** -- edit within a time window, broadcast edit as gossip update
- **Reactions** -- extend likes to custom reactions (emoji reactions or predefined set)
- **Lists** -- user-curated lists of accounts (like Twitter lists), each list is a custom feed
- **Mute/block** -- local mute (hide posts) and block (refuse connections), never broadcast
- **Content warnings / spoiler tags** -- optional CW field on posts, collapsed by default
- **Pinned posts** -- pin a post to the top of your profile
- **Scheduled posts** -- compose now, publish later (local timer)
- **Drafts** -- save unfinished posts locally
- **Post expiry / ephemeral posts** -- auto-delete after N hours, broadcast DeletePost on timer
- **Thread composer** -- write multi-post threads in one go, publish as linked reply chain
- **Link previews** -- fetch Open Graph metadata for URLs in posts, display card preview
- **Markdown in posts** -- render basic markdown (bold, italic, code, links) in post content
- **Translation** -- on-demand translation of posts via local or remote API

---

## Messaging and Communication

- ~~**Direct messages**~~ -- DONE: E2E encrypted with Noise IK + Double Ratchet, offline queuing, delivery status
- **Voice calls** -- see dedicated design doc
- **Video calls** -- see dedicated design doc
- **Group chats** -- multi-party encrypted messaging (shared ratchet or sender keys)
- **Disappearing messages** -- auto-delete DMs after read + timer
- **Voice messages** -- record and send audio clips as DM attachments
- **Message reactions** -- react to individual DM messages
- ~~**Read receipts**~~ -- DONE: sent back to peer on conversation open, displayed in UI
- ~~**Typing indicators**~~ -- DONE: debounced input events sent over encrypted channel

---

## Community and Discovery

- **Discovery server** -- see dedicated design doc
- **User search** -- search by display name or pubkey (local + server-assisted)
- **Suggested follows** -- "friends of friends" discovery from your follow graph
- **Who to follow** -- recommend accounts based on shared follows
- **Explore page** -- trending posts, trending tags, popular accounts (server-powered)
- **User directories** -- browse registered users on a discovery server
- **Invite links** -- generate a shareable link containing your node ID + relay info
- **QR codes** -- display/scan QR code to follow someone in person
- **Follow requests** -- optional approval before someone can follow you
- **Mutual follow indicator** -- show when two users follow each other
- **Follow categories / circles** -- organize follows into groups, filter feed by group

---

## Media and Content

- **Image galleries** -- swipeable image viewer for multi-image posts
- **Video playback** -- inline video player for video attachments
- **Audio player** -- inline player for audio attachments (podcasts, music clips)
- **GIF support** -- search and insert GIFs (via tenor/giphy API or local library)
- **Camera integration** -- take photo/video directly from the app
- **Image editing** -- crop, rotate, filters before posting
- **Media compression** -- auto-compress images/videos before uploading as blobs
- **Thumbnail generation** -- generate and cache thumbnails for large images
- **PDF/document sharing** -- attach documents, preview first page
- **Code snippets** -- syntax-highlighted code blocks in posts
- **Location sharing** -- attach coordinates to a post, display on map
- **Music sharing** -- share what you're listening to (Spotify/local metadata)

---

## Feed and Timeline

- **Algorithmic feed** -- optional relevance-sorted feed (local algorithm, not server-controlled)
- **Chronological feed toggle** -- switch between algorithmic and chronological
- **Feed filters** -- filter by: posts only, posts+replies, media only, links only
- **Muted words** -- hide posts containing specific words/phrases
- **Custom feeds** -- create feeds from specific lists or hashtags
- ~~**Unified notifications feed**~~ -- DONE: /notifications route with likes, replies, reposts, new followers
- **Read position sync** -- remember scroll position, mark "caught up" point
- **Feed grouping** -- group consecutive posts from same author
- **Repost deduplication** -- if multiple follows repost the same thing, show once with "X and Y reposted"

---

## Profile and Identity

- **Profile banner image** -- large header image on profile page
- **Profile links** -- add website, GitHub, etc. links to profile
- **Profile fields** -- custom key-value fields (like Mastodon)
- **Identity verification** -- link to external proofs (domain, GitHub, Keybase-style)
- **Multiple identities** -- switch between different keypairs/personas in one app
- **Account export/import** -- export full account (keys, follows, posts) as encrypted file
- **Account migration** -- move identity to new keypair with signed migration notice
- **Profile badges** -- visual indicators (early adopter, discovery server admin, etc.)
- **Activity status** -- show online/offline/away status (opt-in)
- **Profile analytics** -- view stats on your own posts (local counts of interactions received)

---

## Networking and Protocol

- **Relay server list** -- configure multiple relay servers, auto-fallback
- **Custom relay hosting** -- documentation and tooling for running your own relay
- **Connection quality indicator** -- show latency/throughput to peers
- **Bandwidth usage stats** -- track data sent/received per peer, total
- **Offline mode** -- queue posts while offline, publish when reconnected
- **Selective sync** -- only sync posts newer than N days to save storage
- **Blob garbage collection** -- clean up unreferenced blobs, reclaim disk space
- **Blob pinning** -- keep specific blobs even if GC would remove them
- **Peer reputation** -- track reliability of peers (uptime, valid data ratio)
- **Protocol versioning** -- graceful upgrade path for breaking protocol changes
- **Tor/I2P support** -- route connections through anonymizing networks
- **Multi-device sync** -- sync account state across multiple devices (same keypair)
- **Gossip topic compression** -- reduce redundant gossip traffic between peers
- **Batch sync** -- sync multiple follows in a single connection round-trip

---

## Security and Privacy

- **End-to-end encrypted DMs** -- see DM design doc
- **Key rotation** -- rotate identity key with signed migration
- **2FA for key access** -- protect secret key with additional factor
- **Encrypted local storage** -- encrypt the SQLite database at rest
- **Plausible deniability** -- hidden accounts behind secondary passphrase
- **Anonymous posting** -- post without attaching identity (server-assisted mixing)
- **Spam filtering** -- heuristic spam detection (repeated content, rapid posting)
- **Proof of work** -- require small PoW for posting to deter spam (opt-in per server)
- **Rate limiting** -- local rate limit on outgoing posts to prevent accidents
- **Content reporting** -- report posts to discovery server moderators
- **Moderation tools** -- server-side moderation (hide posts, ban users from server index)
- **Allowlist mode** -- only accept connections/messages from followed users

---

## UI/UX

- **Dark/light theme toggle** -- currently dark only, add light theme
- **Custom themes** -- user-defined color schemes
- **Font size adjustment** -- accessibility setting for text size
- **Compact/comfortable view modes** -- toggle between dense and spacious layouts
- **Gesture navigation** -- swipe to go back, pull to refresh
- **Keyboard shortcuts** -- power user keybinds (j/k for next/prev post, l to like, etc.)
- **Right-click context menus** -- copy link, copy text, mute user, etc.
- ~~**Toast notifications**~~ -- DONE: non-intrusive success/error feedback
- **Skeleton loading states** -- show placeholder shapes while content loads
- **Empty states** -- friendly illustrations/messages when feeds are empty
- ~~**Onboarding flow**~~ -- DONE: first-run setup (set display name, bio, avatar)
- **Tutorial tooltips** -- explain P2P concepts for new users
- **Accessibility** -- screen reader support, ARIA labels, high contrast mode
- **Animations** -- subtle transitions for post appearing, like heart animation
- **Pull to refresh** -- standard mobile-style refresh gesture
- **Infinite scroll improvements** -- smoother loading, scroll position preservation
- **Multi-column layout** -- wide screens show feed + detail side by side (Tweetdeck style)
- **Mobile responsive** -- adapt layout for narrow screens if used in mobile webview
- **System tray** -- minimize to tray, show notification badges
- **Desktop notifications** -- OS-level notifications for mentions, DMs, new followers
- ~~**Unread count badge**~~ -- show unread count on app icon / nav tabs
- ~~**Confirmation dialogs**~~ -- DONE: confirm before delete, unfollow, etc.
- ~~**Unread count badge**~~ -- DONE: unread DM count badge on Messages nav tab

---

## Developer and Power User

- **Plugin system** -- loadable extensions that can add commands, UI panels, filters
- **Bot framework** -- create automated accounts (news bots, bridge bots, reminder bots)
- **API / CLI tool** -- command-line interface for scripting (post, follow, export)
- **Webhook integration** -- trigger HTTP webhooks on events (new post, new follower)
- **RSS bridge** -- publish your feed as RSS, or import RSS feeds as posts
- **ActivityPub bridge** -- bidirectional bridge to Mastodon/fediverse
- **Nostr bridge** -- bidirectional bridge to Nostr network
- **Bluesky bridge** -- bridge to AT Protocol / Bluesky
- **Matrix bridge** -- bridge DMs to Matrix rooms
- **Export to blog** -- publish selected posts as a static blog/website
- **Data export** -- export all your data (posts, follows, interactions) as JSON/CSV
- **Debug panel** -- view gossip traffic, connection states, peer list, blob store stats
- **Log viewer** -- view and filter application logs in the UI
- **Performance profiler** -- track and display sync times, gossip latency, storage size

---

## Platform and Distribution

- **Mobile app (Android)** -- Tauri 2 supports Android targets
- **Mobile app (iOS)** -- Tauri 2 supports iOS targets
- **Flatpak packaging** -- distribute on Flathub for Linux
- **Snap packaging** -- distribute via Snap Store
- **AppImage** -- portable Linux binary
- **Homebrew formula** -- install via Homebrew on macOS
- **Windows installer** -- MSI/NSIS installer for Windows
- **One-click server deploy** -- DigitalOcean / Vultr / Linode marketplace images for discovery server
- **Auto-updates** -- check for and install updates (Tauri updater plugin)
- **Web client** -- browser-based client connecting to a local or remote node
- **PWA** -- progressive web app version for lightweight access

---

## Content and Ecosystem

- **Decentralized wiki** -- collaborative documents shared via blobs
- **File sharing** -- share arbitrary files via blob tickets (like a P2P Dropbox)
- **Collaborative playlists** -- shared music/media playlists
- **Events** -- create and RSVP to events, shared via posts
- **Marketplace** -- P2P classifieds / buy-sell-trade listings
- **Tipping / payments** -- send cryptocurrency tips on posts (Lightning, etc.)
- **Prediction markets** -- peer-to-peer predictions on events
- **Decentralized forum** -- topic-based discussion boards (like Reddit communities)
- **Collaborative documents** -- real-time co-editing via CRDT over iroh connections
- **Photo albums** -- curated collections of images, shared as a unit
- **Stories / ephemeral content** -- posts that auto-expire after 24 hours
- **Spaces / rooms** -- live audio rooms (Clubhouse/Twitter Spaces style) via group calls
- **Newsletter** -- long-form posts distributed to subscribers
- **Blog mode** -- profile page renders as a blog with post titles and reading time
