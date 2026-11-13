# Privacy Policy

Last updated: 2026-11-12

This policy describes the data the Proscenium app processes, where that data is sent, and the choices you have. Proscenium is developed by an individual developer ("we", "us"). Contact: [CONTACT EMAIL].

## Summary

- Proscenium has no central account system. Your identity is a key pair created and stored on your device.
- Content you publish is sent from your device to the devices of other users. We do not receive it unless you register with a discovery server we operate.
- The app contains no advertising, analytics or tracking code. We do not sell or share data for advertising.
- Registration with a discovery server is optional.

## Data stored on your device

The app stores the following in its private storage on your device:

- your identity: the recovery phrase, the master key derived from it, and a key for each device;
- your profile, posts, replies, likes, reposts, bookmarks, follows, followers, and the users you muted or blocked;
- direct messages you sent and received, and media files;
- content received from other users;
- settings and diagnostic logs.

This data is excluded from Android cloud backups and device-to-device transfers. Uninstalling the app deletes it from the device. "Delete all data" in Preferences deletes it from the device except your identity (recovery phrase and keys), your profile, the settings for sharing your follow lists, the list of your devices, and the diagnostic logs; your other devices then copy your follows, messages and posts back at the next device sync. Data already sent to other devices or servers is not deleted from them.

## Data you publish

Your profile visibility controls who receives your content:

- Public: your profile (display name, bio, avatar), posts, replies, media, likes and reposts are available to any user who requests them, from your devices, from devices that hold a copy, and from discovery servers where you are registered.
- Listed: your profile is available to anyone and can be listed by discovery servers. Posts are sent only to followers you approved.
- Private: posts are sent only to users who follow you and whom you follow. Discovery servers do not list you.

The lists of accounts you follow and of your followers are shared with the users who can receive your posts, unless you turn sharing off in Preferences. The list of your linked devices (their network identifiers) is signed with your identity and shared with other users so that they can connect to your devices.

Media files (images and other attachments, also those of Listed or Private posts and of direct messages) are served by your device to any device that requests them by their content identifier, which is part of the post or message.

When none of your devices is reachable and your profile is Public, devices of your followers can send your Public profile, posts and post deletions to other users who follow you. Your device does the same for Public users you follow, if your own profile is Public and you share your follow list.

When you delete a post or undo a like or repost, the app sends a signed deletion notice. Devices and servers running Proscenium delete the content when they receive the notice. Copies saved by other means, such as screenshots, are not affected.

## Direct messages

Messages are sent over encrypted connections (QUIC with TLS 1.3) between your device and the recipient's devices, either directly or through a relay server that forwards the encrypted traffic and cannot read it. Messages are stored on the sender's and the recipient's devices. While the recipient's devices are offline, a message is kept in the outbox on your device. We do not receive or store messages.

## Calls and stages

Call and stage audio is sent over encrypted connections between the participants' devices, either directly or through relay servers. The app does not record audio. In a stage, all participants receive the speakers' audio, the chat messages and the reactions, and see which identities are present.

## Network services

To connect devices, the app uses these services:

- Relay servers operated by number 0 (n0.computer), the developer of the iroh networking library used by the app. Your device keeps a connection to a relay server. The relay server receives your IP address and your device's network identifier, and forwards encrypted traffic when a direct connection is not possible.
- Address lookup operated by number 0. The app publishes a signed record with your device's network identifier and relay server address to number 0's DNS server, and queries that server to find other devices.
- Mainline DHT, off by default. When you turn it on in Preferences, the app also publishes that record to the public BitTorrent DHT, where anyone can read it.
- Local network discovery, off by default. When you turn it on in Preferences, the app announces your device's network identifier to other devices on your local network (mDNS).
- Router port mapping. The app asks your router (UPnP, NAT-PMP or PCP) to forward a port to your device, so that other devices can connect directly.

Every device your device connects to receives your IP address.

## Discovery servers

Registration with a discovery server is optional, possible for Public and Listed profiles, and can be undone on the Servers page. The setup screen offers registration with discovery.identia.app, which we operate; the option is switched on for Public and Listed profiles and can be switched off before you continue. When you register, the server stores:

- your public key, the time of registration and the time of your last request;
- your profile: display name, bio, avatar identifier and visibility;
- the network identifiers of your devices;
- for a Public profile: your posts, replies, likes and reposts, which the server indexes for search, trending topics and its feed.

The server makes this data available to anyone who uses its interface. It uses your IP address to limit the request rate and does not write it to its database. When you unregister, the server deletes your profile, device identifiers, posts, likes and reposts. It keeps your public key and the timestamps for 30 days to reject replayed requests, then deletes them.

Discovery servers operated by others follow the policies of their operators.

## Android permissions

- Camera: scans QR codes for linking devices and following users, only while the scanner is open. Images are not stored or sent.
- Microphone: sends your voice in calls and while you speak in a stage, only during the call or stage.
- Notifications: the app shows notifications for direct messages and mentions. No push notification service is used.
- Network access and network state: connects to other devices and detects network changes.

Media you attach is read from the files you select.

## Children

Proscenium is not directed at children under 13, and users must be at least 13 years old. If you believe a child under 13 registered with discovery.identia.app, contact us and we will delete the registration.

## Security

Keys are stored in the app's private storage. Content is signed with your identity key, and connections between devices are encrypted. No method of storage or transmission is completely secure.

## Your choices

- Choose your profile visibility and approve or deny follow requests.
- Turn sharing of your follow lists, the Mainline DHT and local network discovery on or off in Preferences.
- Unregister from a discovery server on the Servers page.
- Block or mute users.
- Remove a linked device in Preferences.
- Delete the data of a device in Preferences, or uninstall the app to delete everything on that device.

## Requests

We hold your data only if you registered with discovery.identia.app. You can delete that data by unregistering. For other requests concerning that server, contact [CONTACT EMAIL].

## Changes

When this policy changes, we publish the new version with a new date.

## Contact

[CONTACT EMAIL]
