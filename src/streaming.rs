pub(crate) fn string_to_digit_stream(str: &str) -> impl Iterator<Item = u8> + '_ {
    str.bytes()
        .map(|b| char::from(b).to_digit(10).unwrap() as u8)
}

pub(crate) fn digit_stream_to_string(digits: impl Iterator<Item = u8>) -> String {
    digits
        .map(|d| char::from_digit(d as u32, 10).unwrap() as u8)
        .collect::<Vec<u8>>()
        .iter()
        .map(|b| char::from(*b))
        .collect()
}

pub(crate) fn streaming_midpoint(
    x: impl Iterator<Item = u8>,
    y: impl Iterator<Item = u8>,
) -> impl Iterator<Item = u8> {
    streaming_halve(streaming_add(x, y))
}

pub(crate) fn streaming_add(
    mut x: impl Iterator<Item = u8>,
    mut y: impl Iterator<Item = u8>,
) -> impl Iterator<Item = u8> {
    let mut prev = None;
    let mut nines = 0;
    let mut overflown = false;
    std::iter::from_fn(move || {
        if nines > 0 {
            nines -= 1;
            if overflown {
                Some(0)
            } else {
                Some(9)
            }
        } else {
            loop {
                match (x.next(), y.next()) {
                    (Some(x), Some(y)) => match x + y {
                        0..9 => {
                            overflown = false;
                            let res = prev.replace(x + y);
                            if res.is_none() {
                                // first iteration
                                continue;
                            } else {
                                break res;
                            }
                        }
                        9 => {
                            nines += 1;
                        }
                        10..19 => {
                            overflown = true;
                            break Some(prev.replace((x + y) % 10).unwrap_or_default() + 1);
                        }
                        _ => unreachable!(),
                    },
                    (None, None) => {
                        overflown = false;
                        break prev.take();
                    }
                    _ => unreachable!("x and y must have the same length"),
                }
            }
        }
    })
}

fn streaming_halve(mut x: impl Iterator<Item = u8>) -> impl Iterator<Item = u8> {
    let mut carry = false;
    let mut finished = false;
    std::iter::from_fn(move || match (x.next(), finished) {
        (_, true) => None,
        (None, false) => {
            finished = true;
            if carry {
                Some(5)
            } else {
                None
            }
        }
        (Some(x), false) => {
            let d = x / 2 + if carry { 5 } else { 0 };
            carry = x % 2 == 1;
            Some(d)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn test_streaming_add(x in 0u32..10000, y in 0u32..10000) {
            println!();
            let x_str = format!("{:0>5}", x);
            println!("   x: {}", x_str);
            let y_str = format!("{:0>5}", y);
            println!("   y: {}", y_str);
            let digits = streaming_add(
                string_to_digit_stream(&x_str),
                string_to_digit_stream(&y_str),
            );
            let res = digit_stream_to_string(digits);
            println!(" x+y: {}", res);
            let real_str = format!("{:0>5}", x + y);
            println!("real: {}", real_str);
            prop_assert_eq!(res, real_str);
        }

        #[test]
        fn test_streaming_halve(x in 0u32..20000) {
            println!();
            let x_str = format!("{:0>5}", x);
            println!("   x: {}", x_str);
            let digits = streaming_halve(string_to_digit_stream(&x_str));
            let res = digit_stream_to_string(digits);
            println!(" x/2: {}", res);
            let real_str = format!("{:0>5}{}", x / 2, if x % 2 == 0 { "" } else { "5" });
            println!("real: {}", real_str);
            prop_assert_eq!(res, real_str);
        }

        #[test]
        fn test_streaming_midpoind(init in 0u32..10000, diff in 1u32..10000) {
            println!();
            let x_str = format!("{:0>5}", init);
            println!("      x: {}", x_str);
            let y_str = format!("{:0>5}", init + diff);
            println!("      y: {}", x_str);
            let digits = streaming_midpoint(
                string_to_digit_stream(&x_str),
                string_to_digit_stream(&y_str),
            );
            let res = digit_stream_to_string(digits);
            println!("(x+y)/2: {}", res);
            let real = (init + init + diff) / 2;
            let even = (init + init + diff) % 2 == 0;
            let real_str = format!("{:0>5}{}", real, if even { "" } else { "5" });
            println!("   real: {}", real_str);
            prop_assert_eq!(res, real_str);
        }
    }
}
