use std::mem::size_of;

use ring::aead::{
    Aad, BoundKey, Nonce, NonceSequence, OpeningKey, SealingKey, Tag, UnboundKey, AES_256_GCM,
    NONCE_LEN,
};

use crate::common::checksum::MSG_HEADER_KEY;

#[derive(Clone, Copy, Default)]
struct Counter(u32);

impl Counter {
    fn advance(&mut self) {
        self.0 += 1;
    }

    const fn size() -> usize {
        size_of::<u32>()
    }

    fn to_bytes(self) -> [u8; 4] {
        self.0.to_be_bytes()
    }
}

#[derive(Clone, Copy, Default)]
struct CounterNonceSequence(Counter, [u8; NONCE_LEN]);

type RingResult<T> = Result<T, ring::error::Unspecified>;

impl NonceSequence for CounterNonceSequence {
    // called once for each seal operation
    fn advance(&mut self) -> RingResult<Nonce> {
        let nonce_bytes = &mut self.1;

        let bytes = self.0.to_bytes();
        nonce_bytes[NONCE_LEN - Counter::size()..].copy_from_slice(&bytes);

        self.0.advance(); // advance the counter
        Ok(Nonce::assume_unique_for_key(*nonce_bytes))
    }
}

pub struct Aes256GcmCodec {
    seal: Aes256GcmEnCodec,
    open: Aes256GcmDeCodec,
}

impl Aes256GcmCodec {
    pub fn try_new(key: &[u8]) -> RingResult<Self> {
        Ok(Self {
            seal: Aes256GcmEnCodec::try_new(key)?,
            open: Aes256GcmDeCodec::try_new(key)?,
        })
    }

    pub fn try_new_with_default_key() -> RingResult<Self> {
        Aes256GcmCodec::try_new(MSG_HEADER_KEY.0.as_ref())
    }

    pub fn encrypt(&mut self, data: &mut [u8]) -> RingResult<Tag> {
        self.seal.encrypt(data)
    }

    pub fn decrypt_with_tag(
        &mut self,
        decrypeted_data: &[u8],
        tag: Tag,
    ) -> RingResult<(Vec<u8>, usize)> {
        self.open.decrypt_with_tag(decrypeted_data, tag)
    }

    pub fn decrypt<'a>(&mut self, data: &'a mut [u8]) -> RingResult<&'a mut [u8]> {
        self.open.decrypt(data)
    }
}

#[derive(Debug)]
pub struct Aes256GcmEnCodec {
    seal: SealingKey<CounterNonceSequence>,
}

impl Aes256GcmEnCodec {
    pub fn try_new(key: &[u8]) -> RingResult<Self> {
        let counter = CounterNonceSequence::default();
        Ok(Self {
            seal: SealingKey::new(UnboundKey::new(&AES_256_GCM, key)?, counter),
        })
    }

    pub fn new(key: UnboundKey) -> Self {
        let counter = CounterNonceSequence::default();
        Self {
            seal: SealingKey::new(key, counter),
        }
    }
}

impl Encryptor for Aes256GcmEnCodec {
    fn encrypt(&mut self, data: &mut [u8]) -> RingResult<Tag> {
        self.seal.seal_in_place_separate_tag(Aad::empty(), data)
    }
}

#[derive(Debug)]
pub struct Aes256GcmDeCodec {
    open: OpeningKey<CounterNonceSequence>,
}

impl Aes256GcmDeCodec {
    pub fn try_new(key: &[u8]) -> RingResult<Self> {
        let counter = CounterNonceSequence::default();
        Ok(Self {
            open: OpeningKey::new(UnboundKey::new(&AES_256_GCM, key)?, counter),
        })
    }

    pub fn new(key: UnboundKey) -> Self {
        let counter = CounterNonceSequence::default();
        Self {
            open: OpeningKey::new(key, counter),
        }
    }
}

impl Decryptor for Aes256GcmDeCodec {
    fn decrypt_with_tag(
        &mut self,
        decrypeted_data: &[u8],
        tag: Tag,
    ) -> RingResult<(Vec<u8>, usize)> {
        let mut new_data = [decrypeted_data, tag.as_ref()].concat();
        let new_data_len = self.decrypt(&mut new_data)?.len();
        Ok((new_data, new_data_len))
    }

    fn decrypt<'a>(&mut self, data: &'a mut [u8]) -> RingResult<&'a mut [u8]> {
        self.open.open_in_place(Aad::empty(), data)
    }
}

pub trait Encryptor: 'static {
    fn encrypt(&mut self, data: &mut [u8]) -> RingResult<Tag>;
}

pub trait Decryptor {
    fn decrypt_with_tag(
        &mut self,
        decrypeted_data: &[u8],
        tag: Tag,
    ) -> RingResult<(Vec<u8>, usize)>;
    fn decrypt<'a>(&mut self, data: &'a mut [u8]) -> RingResult<&'a mut [u8]>;
}

#[cfg(test)]
mod tests {
    use std::slice::from_raw_parts_mut;
    use std::time::Instant;

    use crate::utils::codec::Aes256GcmCodec;

    struct Timer {
        ins: Instant,
        hint: String,
    }

    impl Timer {
        fn new_with_hint(hint: String) -> Self {
            Self {
                ins: Instant::now(),
                hint,
            }
        }
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            println!("{} consume time:{:?}", self.hint, self.ins.elapsed());
        }
    }

    fn write_slice(slice: &[u8]) {
        let ptr = slice.as_ptr() as *mut u8;
        let mut_slice = unsafe { from_raw_parts_mut(ptr, slice.len()) };
        mut_slice[1] = b'a';
        println!("{mut_slice:?}")
    }

    #[test]
    fn test_write_mut_slice() {
        let vec: Vec<u8> = "bbc".into();
        write_slice(&vec);
        assert_eq!(vec[1], b'a');
    }

    #[test]
    fn test_codec() {
        let  data = String::from("fdafas反对fdasfasfasfsdafdasfsdfasd范德萨发顺🤣❤️😁😍👍👍丰十大大师傅士大夫大撒发射点发士大夫大师傅大师傅士大夫士大夫阿斯蒂芬大师傅阿斯顿法大师傅看叫阿三的发就可是大家发开始打客服开始大幅喀什的开发点卡收费就开始打客服就是的咖啡肯定撒法开始打客服就是的咖啡就开始大幅扣税的急啊看发叫阿三的发生的开发就是大家可是大家发看大数据开发大数据开发大家ask发就是的咖啡的萨芬就卡死的房价开始打家开发商的JFK上的飞机卡上的纠纷开始打飞机宽带技术开发就开始大家开发建设的卡JFK大数据风控静安寺的看法角度看萨芬卡上的纠纷看静安寺的看法角度思考积分可是大家发卡是大家看法就大肆砍伐尽快打算减肥肯定是积分开始大幅技术大咖积分开始打飞机扣税的急啊看发的技术开发就是JFK十大福克斯大家开发大撒发射点幅度萨芬撒旦发发收范德萨发顺丰士大夫十大阿斯蒂芬大师傅阿斯顿附件是的客服对接撒巨大石块积分的课时费阿斯蒂芬法大师傅大师傅十大法大师傅阿斯蒂芬阿斯顿法大师傅阿斯蒂芬大师傅阿斯顿法大师傅大师傅阿斯蒂芬阿斯蒂芬士大夫阿斯蒂芬大师傅的萨芬打算减肥上岛咖啡加快速度大数据开发就是打客服看大数据开发就开始减肥卡萨丁JFK是大家看法加快速度JFK技术大咖积分喀什的开发独守空房技术大咖积分空手道解放扣税的开发商的开发接口是大家看法角度看是否扣税的急啊看发生的开发的快速减肥开始大幅就是打客服卡上的纠纷啊撒旦解放扣税的急啊看发加快速度点卡JFK啥的但是法大师傅技术大咖积分卡萨丁就反馈是大家看法啊是大家看法卡上的纠纷可是大家发喀什的开发大卡司喀什的开发就是打客服法大师傅士大夫的式咖啡机上岛咖啡就是的咖啡艰苦大师傅看上雕刻技法喀什的开发上岛咖啡就喀什的开发就是打客服卡上的纠纷技术的咖啡机肯定撒开发啊十大科技开发速度加啊反馈就是的咖啡开始大幅大师傅似的十大放假啊上岛咖啡就可是大家发空间的是否撒旦士大夫的撒娇开发是大家看法大肆砍伐就喀什的开发氨基酸的考虑非军事对抗疗法金克拉撒旦发艰苦拉萨的飞机喀什打开发就可是大家发可是大家看附件卡上的纠纷卡刷点卡技术的咖啡机可是大家发卡是大家看法静安寺的看法就可是大家发卡萨丁就开发商的急啊看飞机迪斯科发技术的咖啡机可是大家发看电视剧开发商大开始打到发大水发大水");
        let mut cryption = Aes256GcmCodec::try_new_with_default_key().unwrap();
        let mut out_buf = data.as_bytes().to_vec();
        let tag = {
            let _timer = Timer::new_with_hint("Encrypt".into());
            cryption.encrypt(&mut out_buf).unwrap()
        };
        println!("tag:{:?}", tag.as_ref());
        let (decrypeted_data, len) = {
            let _timer = Timer::new_with_hint("Decrypt".into());
            cryption.decrypt_with_tag(&out_buf, tag).unwrap()
        };
        assert_eq!(
            data,
            String::from_utf8(decrypeted_data[..len].to_vec()).unwrap()
        );
    }
}
