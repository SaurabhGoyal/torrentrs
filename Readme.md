# Overview
A bittorrent client implementation. Why torrent client? 
- Networking - managing lots of data transfer with multiple peers, both download and upload.
- p2p - both client and server.
- decentralised
- an essential app

# Implementation
Single torrent level
- [x] Read torrent file (metainfo)
- [x] Parse bencoding to know tracker, files and pieces.
- [x] Connect with tracker URL to get list of peers.
- [x] Connect with peers to request pieces.
- [ ] Download pieces, check integrity and write bytes to the disk.
- [ ] On completion, verify integrity of whole data.
- [ ] Seed.

Client level
- [ ] Persist torrents state.
- [ ] CRUD for torrents.
- [ ] TUI.

Peer Connection
- [ ] Event loop / async for better performance.
- [ ] Piece request pipelining at TCP as well as application level.

Server level
- [ ] Acting as a peer that can serve data.
- [ ] Better management for choke and interest actions for both parties.

# References
- Blog posts -
  - https://allenkim67.github.io/programming/2016/05/04/how-to-make-your-own-bittorrent-client.html
  - http://www.kristenwidman.com/blog/33/how-to-write-a-bittorrent-client-part-1/
- Specifications (must read) -
  - [Bittorrent unofficial specification](https://wiki.theory.org/BitTorrentSpecification#Identification)
  - [Bittorrent official specification](http://bittorrent.org/beps/bep_0003.html)
- Similar clients -
  - https://github.com/ikatson/rqbit
