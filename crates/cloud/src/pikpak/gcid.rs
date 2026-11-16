//! PikPak 文件的 GCID（文件对象的 `hash` 字段，迅雷的算法）：按文件大小定块大小，每块 SHA-1，各块的摘要（20 字节原始
//! 值）依次拼起来再做一次 SHA-1，写成 40 位十六进制。只看内容，与文件名、位置无关。

use sha1::{Digest as _, Sha1};

/// 最小的块：256 KiB。
const MIN_PIECE: u64 = 0x40000;
/// 最大的块：2 MiB。
const MAX_PIECE: u64 = 0x20_0000;
/// 块数超过这么多时块大小翻倍（到 [`MAX_PIECE`] 为止）。
const MAX_PIECES: u64 = 0x200;

/// 总长 `size` 字节的文件按多大的块算：从 256 KiB 起，`size` 大于块大小的 512 倍时翻倍，最大 2 MiB。即 128 MiB 及以下
/// 256 KiB，256 MiB 及以下 512 KiB，512 MiB 及以下 1 MiB，更大的 2 MiB。
pub fn piece_size(size: u64) -> u64 {
    let mut piece = MIN_PIECE;
    while u128::from(size) > u128::from(piece) * u128::from(MAX_PIECES) && piece < MAX_PIECE {
        piece <<= 1;
    }
    piece
}

/// 逐段喂入文件内容算 GCID。块大小按建它时给的总长定，喂入的总字节数要等于它，否则算出的不是这个文件的 GCID。
pub struct Gcid {
    piece: u64,
    total: Sha1,
    block: Sha1,
    /// 当前块已喂入的字节数，小于 `piece`
    filled: u64,
}

impl Gcid {
    /// 总长 `size` 字节的文件。
    pub fn new(size: u64) -> Self {
        Gcid {
            piece: piece_size(size),
            total: Sha1::new(),
            block: Sha1::new(),
            filled: 0,
        }
    }

    /// 喂入接着的一段内容。
    pub fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let room = self.piece - self.filled;
            let take = usize::try_from(room).map_or(data.len(), |room| room.min(data.len()));
            let (head, rest) = data.split_at(take);
            self.block.update(head);
            self.filled += head.len() as u64;
            data = rest;
            if self.filled == self.piece {
                self.close_block();
            }
        }
    }

    /// 40 位小写十六进制；没喂入任何内容时为空串的 SHA-1。
    pub fn finish(mut self) -> String {
        if self.filled > 0 {
            self.close_block();
        }
        crate::hex(&self.total.finalize())
    }

    fn close_block(&mut self) {
        let block = std::mem::replace(&mut self.block, Sha1::new());
        self.total.update(block.finalize());
        self.filled = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * 31 + 7) % 251) as u8).collect()
    }

    /// 期望值由照 xlgcid 0.1.4 的写法、只用 Python 标准库的实现算出。
    #[test]
    fn matches_reference_values() {
        let cases = [
            (0, "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            (1, "8aacfde27b33a25ea9797815353fa580f744b3cb"),
            (262_144, "b21dcbd82929f47758eb01e52490f8a500d96d6c"),
            (262_145, "6805ed701e6ea96597858628f8999a955d441d9d"),
            (1_048_579, "92c310e1038a4c106fd2a55c8695156b8565589f"),
        ];
        for (len, expected) in cases {
            let content = data(len);
            let mut whole = Gcid::new(len as u64);
            whole.update(&content);
            assert_eq!(whole.finish(), expected, "{len} 字节一次喂入");
            // 分段喂入（段与块不对齐）结果相同
            let mut pieces = Gcid::new(len as u64);
            for chunk in content.chunks(100_003) {
                pieces.update(chunk);
            }
            assert_eq!(pieces.finish(), expected, "{len} 字节分段喂入");
        }
    }

    /// 刚超过 512 块即翻倍：128 MiB 多 1 字节已是 512 KiB。
    #[test]
    fn piece_doubles_just_past_each_boundary() {
        const MIB: u64 = 1 << 20;
        let cases = [
            (0, 256 * 1024),
            (128 * MIB, 256 * 1024),
            (128 * MIB + 1, 512 * 1024),
            (256 * MIB, 512 * 1024),
            (256 * MIB + 1, MIB),
            (512 * MIB, MIB),
            (512 * MIB + 1, 2 * MIB),
            (10 << 30, 2 * MIB),
            (u64::MAX, 2 * MIB),
        ];
        for (size, piece) in cases {
            assert_eq!(piece_size(size), piece, "{size} 字节");
        }
    }
}
