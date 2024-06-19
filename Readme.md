# Overview
A bittorrent client implementation. Why torrent client? 
- Networking - managing lots of data transfer with multiple peers, both download and upload.
- p2p - both client and server.
- decentralised
- an essential app

# Design
- Torrents -
  - Entry points, 1st class citizen. Has (info_hash, files, pieces, peers).
  - Consists of pieces that need to be downloaded from peers.
- Peers -
  - Needed to get torrents. has (ip, port).
  - A pper connection is made using TCP is 1:1 with peer.
  - Although connection is torrent agnostic, a handshake is needed per torrent file before exchanging any peer protocol messages, hence I would treat connection to be mapped 1:1 with a torrent.
  - So Torrent <1:N> Peer <1:1> Connection.
- Pieces -
  - Made of (index, hash, length)
  - Available at zero or more peers.
  - So Torrent <1:M> Piece <1:L> Peer/Connection.
  - One piece can be available at multiple peers.

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
- [ ] DHT.

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
