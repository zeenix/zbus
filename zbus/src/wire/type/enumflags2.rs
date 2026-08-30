use crate::wire::{Signature, Type};

// BitFlags
impl<F> Type for enumflags2::BitFlags<F>
where
    F: enumflags2::BitFlag,
    F::Numeric: Type,
{
    const SIGNATURE: &'static Signature = F::Numeric::SIGNATURE;
}

#[cfg(test)]
mod tests {
    use enumflags2::{BitFlags, bitflags};

    use super::*;

    #[bitflags]
    #[repr(u16)]
    #[derive(Copy, Clone)]
    enum Flags {
        One = 1,
    }

    #[test]
    fn signature_comes_from_numeric_type() {
        fn assert_type<T: Type>() {}

        assert_type::<BitFlags<Flags>>();
        assert_eq!(BitFlags::<Flags>::SIGNATURE, u16::SIGNATURE);
    }
}
