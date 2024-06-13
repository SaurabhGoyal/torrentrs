# Overview
A bittorrent client implementation. Following the guides given at https://allenkim67.github.io/programming/2016/05/04/how-to-make-your-own-bittorrent-client.html and http://www.kristenwidman.com/blog/33/how-to-write-a-bittorrent-client-part-1/. The [bittorrent unofficial specification](https://wiki.theory.org/BitTorrentSpecification#Identification) is critical to get started.

# Implementation
- [x] Read torrent file (metainfo)
- [x] Parse bencoding to know tracker, files and pieces.
- [ ] Connect with tracker URL to get list of peers.
- [ ] Connect with peers to request pieces.
- [ ] Download pieces, check integrity and write bytes to the disk.
- [ ] On completion, verify integrity of whole data.
- [ ] Seed.