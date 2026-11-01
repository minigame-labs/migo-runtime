#![doc = include_str!("../README.md")]

use num_traits::{Float, FloatConst};

/// Enumeration of the available window functions.
#[derive(Debug, Clone, Copy)]
pub enum WindowFunction {
    /// Blackman window: A taper formed by using the first three terms
    /// of a summation of cosines. Good for low sidelobe level.
    Blackman,
    /// Blackman-Harris window: A generalization of the Blackman window,
    /// offering improved spectral leakage performance.
    BlackmanHarris,
    /// Hamming window: A raised cosine window,
    /// minimizing the nearest side lobe, widely used for spectrum analysis.
    Hamming,
    /// Hann window: A window function that represents a single cosine taper,
    /// minimizing the width of the main lobe.
    Hann,
    /// Nuttall window: A window function with very low side lobes,
    /// good for applications requiring high dynamic range.
    Nuttall,
    /// Blackman-Nuttall window: A window function combining Blackman and Nuttall,
    /// providing low side lobes.
    BlackmanNuttall,
    /// Flat top window: A window designed for accurate amplitude measurements
    /// with very low ripple in the passband.
    /// [More Info](https://www.mathworks.com/help/signal/ref/flattopwin.html)
    FlatTop,
    /// Bartlett window: A triangular window that is zero at each end,
    /// tapering linearly to a peak in the middle.
    Bartlett,
    /// Triangular window: Similar to the Bartlett window but not necessarily zero at the edges,
    /// useful for simple applications.
    Triangular,
    /// Rectangular window: A simple window function with no tapering,
    /// equivalent to not windowing at all.
    Rectangular,
    /// Kaiser window: A parameterized window function that provides a trade-off
    /// between main lobe width and side lobe level.
    /// The `beta` parameter controls the shape of the window.
    Kaiser { beta: f32 },
}

enum WindowFamily {
    Cosine,
    Triangular,
    Rectangular,
    Kaiser,
}

/// Enum to specify the symmetry of a window function, which determines its application in signal processing.
#[derive(Debug, Clone, Copy)]
pub enum Symmetry {
    /// Generate a periodic window:
    /// This window is designed to repeat itself, making it ideal for spectral analysis where the window must seamlessly align with the periodic signal.
    Periodic,
    /// Generate a symmetric window:
    /// This window is symmetric around its center, making it suitable for filter design where a zero-phase response is desired.
    Symmetric,
}

/// A generic struct for generating various windows.
pub struct GenericWindowIter<T> {
    /// Length of the window to generate.
    length: usize,
    /// Current iteration index.
    index: usize,
    /// Window length as a float.
    /// Generally equal to length for a periodic window, or length-1 for a symmetric.
    len_float: T,
    /// Constant, meaning varies depending on window family.
    const_a: T,
    /// Constant, meaning varies depending on window family.
    const_b: T,
    /// Constant, meaning varies depending on window family.
    const_c: T,
    /// Constant, meaning varies depending on window family.
    const_d: T,
    /// Constant, meaning varies depending on window family.
    const_e: T,
    /// Window family, used to pick which function to call to calculate values.
    family: WindowFamily,
}

impl<T> Iterator for GenericWindowIter<T>
where
    T: Float + FloatConst,
{
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<T> {
        if self.index == self.length {
            return None;
        }
        let val = self.calc_at_index();
        self.index += 1;
        Some(val)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.length - self.index;
        (remaining, Some(remaining))
    }
}

impl<T> ExactSizeIterator for GenericWindowIter<T>
where
    T: Float + FloatConst,
{
    #[inline]
    fn len(&self) -> usize {
        self.length
    }
}

impl<T> GenericWindowIter<T>
where
    T: Float + FloatConst,
{
    fn new_cosine(
        length: usize,
        symmetry: Symmetry,
        const_a: T,
        const_b: T,
        const_c: T,
        const_d: T,
        const_e: T,
    ) -> Self {
        let len_float = match symmetry {
            Symmetry::Periodic => T::from(length).unwrap(),
            Symmetry::Symmetric => T::from(length - 1).unwrap(),
        };
        GenericWindowIter {
            const_a,
            const_b,
            const_c,
            const_d,
            const_e,
            index: 0,
            length,
            len_float,
            family: WindowFamily::Cosine,
        }
    }

    fn new_triangular(length: usize, symmetry: Symmetry, len_offset: usize) -> Self {
        let len_adjusted = match symmetry {
            Symmetry::Periodic => length,
            Symmetry::Symmetric => length - 1,
        };
        let len_float = T::from(len_adjusted).unwrap();
        let const_a = len_float / T::from(2).unwrap();
        let const_b = if len_offset > 0 {
            (len_float + T::from(len_offset + 1 - len_adjusted % 2).unwrap()) / T::from(2).unwrap()
        } else {
            const_a
        };
        GenericWindowIter {
            const_a,
            const_b,
            const_c: T::zero(),
            const_d: T::zero(),
            const_e: T::zero(),
            index: 0,
            length,
            len_float,
            family: WindowFamily::Triangular,
        }
    }

    fn new_rectangular(length: usize) -> Self {
        GenericWindowIter {
            const_a: T::zero(),
            const_b: T::zero(),
            const_c: T::zero(),
            const_d: T::zero(),
            const_e: T::zero(),
            index: 0,
            length,
            len_float: T::zero(),
            family: WindowFamily::Rectangular,
        }
    }

    fn new_kaiser(length: usize, beta: f32, symmetry: Symmetry) -> Self {
        let len_adjusted = match symmetry {
            Symmetry::Periodic => length,
            Symmetry::Symmetric => length - 1,
        };
        let len_float = T::from(len_adjusted).unwrap();
        let const_b = len_float / T::from(2).unwrap();
        GenericWindowIter {
            const_a: T::from(beta).unwrap(),
            const_b,
            const_c: T::zero(),
            const_d: T::zero(),
            const_e: T::zero(),
            index: 0,
            length,
            len_float: T::zero(),
            family: WindowFamily::Kaiser,
        }
    }
    fn calc_at_index(&self) -> T {
        let x_float = T::from(self.index).unwrap();
        match self.family {
            WindowFamily::Cosine => {
                self.const_a
                    - self.const_b
                        * (T::from(2.0).unwrap() * T::PI() * x_float / self.len_float).cos()
                    + self.const_c
                        * (T::from(4.0).unwrap() * T::PI() * x_float / self.len_float).cos()
                    - self.const_d
                        * (T::from(6.0).unwrap() * T::PI() * x_float / self.len_float).cos()
                    + self.const_e
                        * (T::from(8.0).unwrap() * T::PI() * x_float / self.len_float).cos()
            }
            WindowFamily::Triangular => T::one() - ((x_float - self.const_a) / self.const_b).abs(),
            WindowFamily::Rectangular => T::one(),
            WindowFamily::Kaiser => {
                bessel_i0(
                    self.const_a * T::sqrt(T::one() - (x_float / self.const_b - T::one()).powi(2)),
                ) / bessel_i0(self.const_a)
            }
        }
    }
}

/// Simple implementation of the modified Bessel function of order 0
fn bessel_i0<T: Float>(x: T) -> T {
    let base = x * x / T::from(4).unwrap();
    let mut term = T::one();
    let mut result = T::one();
    for idx in 1..1000 {
        term = term * base / T::from(idx * idx).unwrap();
        let previous = result;
        result = result + term;
        if result == previous {
            break;
        }
    }
    result
}

/// Generate an iterator for the values of the selected window function.
///
/// This function creates an iterator that yields the values of the specified window function
/// over the given length, with the chosen symmetry. It supports various window functions
/// commonly used in signal processing to mitigate spectral leakage.
///
/// # Parameters
/// - `length`: The length of the window.
/// - `windowfunc`: The type of window function to generate. See `WindowFunction` enum for options.
/// - `symmetry`: The symmetry of the window function. See `Symmetry` enum for options.
///
/// # Returns
/// An iterator (`GenericWindowIter<T>`) that yields the values of the window function.
///
/// # Type Parameters
/// - `T`: A numeric type that implements the `Float` and `FloatConst` traits.
///
/// # Examples
/// ```
/// use windowfunctions::{window, WindowFunction, Symmetry};
/// let iter = window::<f32>(1024, WindowFunction::Hamming, Symmetry::Symmetric);
/// for value in iter {
///     println!("{}", value);
/// }
/// ```
///
/// # Note
/// Each window function may have different characteristics and uses, so choose the one
/// that best suits your application needs.
pub fn window<T>(
    length: usize,
    windowfunc: WindowFunction,
    symmetry: Symmetry,
) -> GenericWindowIter<T>
where
    T: Float + FloatConst,
{
    match windowfunc {
        WindowFunction::BlackmanHarris => GenericWindowIter::new_cosine(
            length,
            symmetry,
            T::from(0.35875).unwrap(),
            T::from(0.48829).unwrap(),
            T::from(0.14128).unwrap(),
            T::from(0.01168).unwrap(),
            T::zero(),
        ),
        WindowFunction::Blackman => GenericWindowIter::new_cosine(
            length,
            symmetry,
            T::from(0.42).unwrap(),
            T::from(0.5).unwrap(),
            T::from(0.08).unwrap(),
            T::zero(),
            T::zero(),
        ),
        WindowFunction::Hamming => GenericWindowIter::new_cosine(
            length,
            symmetry,
            T::from(0.53836).unwrap(),
            T::from(0.46164).unwrap(),
            T::zero(),
            T::zero(),
            T::zero(),
        ),
        WindowFunction::Hann => GenericWindowIter::new_cosine(
            length,
            symmetry,
            T::from(0.5).unwrap(),
            T::from(0.5).unwrap(),
            T::zero(),
            T::zero(),
            T::zero(),
        ),
        WindowFunction::Nuttall => GenericWindowIter::new_cosine(
            length,
            symmetry,
            T::from(0.3635819).unwrap(),
            T::from(0.4891775).unwrap(),
            T::from(0.1365995).unwrap(),
            T::from(0.0106411).unwrap(),
            T::zero(),
        ),
        WindowFunction::BlackmanNuttall => GenericWindowIter::new_cosine(
            length,
            symmetry,
            T::from(0.3635819).unwrap(),
            T::from(0.4891775).unwrap(),
            T::from(0.1365995).unwrap(),
            T::from(0.0106411).unwrap(),
            T::zero(),
        ),
        WindowFunction::FlatTop => GenericWindowIter::new_cosine(
            length,
            symmetry,
            T::from(0.21557895).unwrap(),
            T::from(0.41663158).unwrap(),
            T::from(0.277263158).unwrap(),
            T::from(0.083578947).unwrap(),
            T::from(0.006947368).unwrap(),
        ),
        WindowFunction::Bartlett => GenericWindowIter::new_triangular(length, symmetry, 0),
        WindowFunction::Triangular => GenericWindowIter::new_triangular(length, symmetry, 1),
        WindowFunction::Rectangular => GenericWindowIter::new_rectangular(length),
        WindowFunction::Kaiser { beta } => GenericWindowIter::new_kaiser(length, beta, symmetry),
    }
}

#[cfg(test)]
mod tests {
    use crate::bessel_i0;
    use crate::window;
    use crate::Symmetry;
    use crate::WindowFunction;
    use num_traits::Float;
    use num_traits::FloatConst;
    use std::fmt::Debug;

    #[test]
    fn test_hann_odd() {
        // reference: scipy.signal.windows.hann(13, sym=True)
        let expected = vec![
            0.0, 0.0669873, 0.25, 0.5, 0.75, 0.9330127, 1.0, 0.9330127, 0.75, 0.5, 0.25, 0.0669873,
            0.0,
        ];
        check_window(WindowFunction::Hann, &expected);
    }

    #[test]
    fn test_hann_even() {
        // reference: scipy.signal.windows.hann(14, sym=True)
        let expected_even = vec![
            0.0,
            0.0572719871733951,
            0.21596762663442215,
            0.4397316598723384,
            0.6773024435212678,
            0.8742553740855505,
            0.985470908713026,
            0.985470908713026,
            0.8742553740855505,
            0.6773024435212679,
            0.43973165987233875,
            0.21596762663442215,
            0.05727198717339521,
            0.0,
        ];
        check_window(WindowFunction::Hann, &expected_even);
    }

    #[test]
    fn test_hamming_odd() {
        // reference: scipy.signal.windows.general_hamming(13, 0.53836, sym=True):
        let expected = vec![
            0.0767199999999999,
            0.1385680325969516,
            0.3075399999999998,
            0.53836,
            0.76918,
            0.9381519674030482,
            1.0,
            0.9381519674030483,
            0.7691800000000002,
            0.53836,
            0.3075400000000002,
            0.13856803259695172,
            0.0767199999999999,
        ];
        check_window(WindowFunction::Hamming, &expected);
    }

    #[test]
    fn test_hamming_even() {
        // reference: scipy.signal.windows.general_hamming(14, 0.53836, sym=True):
        let expected = vec![
            0.0767199999999999,
            0.12959808031745212,
            0.2761185903190292,
            0.4827154469269326,
            0.7020598000543161,
            0.8839025017857071,
            0.9865855805965627,
            0.9865855805965627,
            0.8839025017857072,
            0.7020598000543162,
            0.4827154469269329,
            0.2761185903190292,
            0.12959808031745224,
            0.0767199999999999,
        ];
        check_window(WindowFunction::Hamming, &expected);
    }

    #[test]
    fn test_blackman_odd() {
        // reference: scipy.signal.windows.blackman(13, sym=True)
        let expected = vec![
            -1.3877787807814457e-17,
            0.02698729810778064,
            0.1299999999999999,
            0.34,
            0.6299999999999999,
            0.8930127018922192,
            0.9999999999999999,
            0.8930127018922194,
            0.6300000000000002,
            0.34,
            0.1300000000000002,
            0.026987298107780687,
            -1.3877787807814457e-17,
        ];
        check_window(WindowFunction::Blackman, &expected);
    }

    #[test]
    fn test_blackman_even() {
        // reference: scipy.signal.windows.blackman(14, sym=True)
        let expected = vec![
            -1.3877787807814457e-17,
            0.022717166911887535,
            0.10759923567101926,
            0.2820563144782543,
            0.5374215836675796,
            0.8038983085059763,
            0.9763073907652827,
            0.9763073907652828,
            0.8038983085059764,
            0.5374215836675799,
            0.2820563144782545,
            0.10759923567101926,
            0.022717166911887583,
            -1.3877787807814457e-17,
        ];
        check_window(WindowFunction::Blackman, &expected);
    }

    #[test]
    fn test_blackman_harris_odd() {
        // reference: scipy.signal.windows.blackmanharris(13, sym=True)
        let expected = vec![
            6.0000000000001025e-05,
            0.006518455586096459,
            0.05564499999999996,
            0.21747000000000008,
            0.5205749999999999,
            0.8522615444139033,
            1.0,
            0.8522615444139037,
            0.5205750000000002,
            0.21747000000000008,
            0.05564500000000015,
            0.006518455586096469,
            6.0000000000001025e-05,
        ];
        check_window(WindowFunction::BlackmanHarris, &expected);
    }

    #[test]
    fn test_blackman_harris_even() {
        // reference: scipy.signal.windows.blackmanharris(14, sym=True)
        let expected = vec![
            6.0000000000001025e-05,
            0.005238996226589691,
            0.04261168680481082,
            0.1668602695128325,
            0.4158082954127571,
            0.7346347391691189,
            0.9666910128738909,
            0.966691012873891,
            0.7346347391691193,
            0.4158082954127572,
            0.16686026951283278,
            0.04261168680481082,
            0.005238996226589701,
            6.0000000000001025e-05,
        ];
        check_window(WindowFunction::BlackmanHarris, &expected);
    }

    #[test]
    fn test_nuttall_odd() {
        // reference: scipy.signal.windows.nuttall(13, sym=True)
        let expected = vec![
            0.0003628000000000381,
            0.008241508040237797,
            0.06133449999999996,
            0.22698240000000006,
            0.5292298,
            0.8555217919597622,
            1.0,
            0.8555217919597622,
            0.5292298000000003,
            0.22698240000000006,
            0.06133450000000015,
            0.008241508040237806,
            0.0003628000000000381,
        ];
        check_window(WindowFunction::Nuttall, &expected);
    }

    #[test]
    fn test_nuttall_even() {
        // reference: scipy.signal.windows.nuttall(14, sym=True)
        let expected = vec![
            0.0003628000000000381,
            0.006751452513864563,
            0.04759044606176561,
            0.17576128736842006,
            0.4253782120718732,
            0.7401569329915648,
            0.9674626189925116,
            0.9674626189925118,
            0.7401569329915649,
            0.4253782120718734,
            0.17576128736842025,
            0.04759044606176561,
            0.006751452513864572,
            0.0003628000000000381,
        ];
        check_window(WindowFunction::Nuttall, &expected);
    }

    #[test]
    fn test_flat_top_odd() {
        // reference: scipy.signal.windows.flattop(13, sym=True)
        let expected = vec![
            -0.0004210510000000013,
            -0.01007668729884861,
            -0.05126315599999999,
            -0.05473684,
            0.19821052999999986,
            0.7115503772988484,
            1.000000003,
            0.7115503772988487,
            0.1982105300000003,
            -0.05473684,
            -0.05126315600000008,
            -0.010076687298848712,
            -0.0004210510000000013,
        ];
        check_window(WindowFunction::FlatTop, &expected);
    }

    #[test]
    fn test_flat_top_even() {
        // reference: scipy.signal.windows.flattop(14, sym=True)
        let expected = vec![
            -0.0004210510000000013,
            -0.00836446681676538,
            -0.04346351871731231,
            -0.06805774015843734,
            0.08261602096572271,
            0.5066288028080755,
            0.9321146024187164,
            0.9321146024187167,
            0.5066288028080761,
            0.08261602096572282,
            -0.06805774015843732,
            -0.04346351871731231,
            -0.008364466816765486,
            -0.0004210510000000013,
        ];
        check_window(WindowFunction::FlatTop, &expected);
    }

    #[test]
    fn test_bartlett_odd() {
        // reference: scipy.signal.windows.bartlett(14, sym=True)
        let expected = vec![
            0.0,
            0.16666666666666666,
            0.3333333333333333,
            0.5,
            0.6666666666666666,
            0.8333333333333334,
            1.0,
            0.8333333333333333,
            0.6666666666666667,
            0.5,
            0.33333333333333326,
            0.16666666666666674,
            0.0,
        ];
        check_window(WindowFunction::Bartlett, &expected);
    }

    #[test]
    fn test_bartlett_even() {
        // reference: scipy.signal.windows.bartlett(14, sym=True)
        let expected = vec![
            0.0,
            0.15384615384615385,
            0.3076923076923077,
            0.46153846153846156,
            0.6153846153846154,
            0.7692307692307693,
            0.9230769230769231,
            0.9230769230769231,
            0.7692307692307692,
            0.6153846153846154,
            0.46153846153846145,
            0.3076923076923077,
            0.15384615384615374,
            0.0,
        ];
        check_window(WindowFunction::Bartlett, &expected);
    }

    #[test]
    fn test_triangular_odd() {
        // reference: scipy.signal.windows.triang(13, sym=True)
        let expected = vec![
            0.14285714285714285,
            0.2857142857142857,
            0.42857142857142855,
            0.5714285714285714,
            0.7142857142857143,
            0.8571428571428571,
            1.0,
            0.8571428571428572,
            0.7142857142857142,
            0.5714285714285714,
            0.4285714285714286,
            0.2857142857142858,
            0.1428571428571428,
        ];
        check_window(WindowFunction::Triangular, &expected);
    }

    #[test]
    fn test_triangular_even() {
        // reference: scipy.signal.windows.triang(14, sym=True)
        let expected = vec![
            0.07142857142857142,
            0.21428571428571427,
            0.35714285714285715,
            0.5,
            0.6428571428571429,
            0.7857142857142857,
            0.9285714285714286,
            0.9285714285714286,
            0.7857142857142857,
            0.6428571428571429,
            0.5,
            0.35714285714285715,
            0.21428571428571427,
            0.07142857142857142,
        ];
        check_window(WindowFunction::Triangular, &expected);
    }

    #[test]
    fn test_rectangular() {
        let expected = vec![
            1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
        ];
        check_window(WindowFunction::Rectangular, &expected);
    }

    #[test]
    fn test_kaiser_odd() {
        // reference: scipy.signal.windows.kaiser(13, 4.0, sym=True)
        let expected = vec![
            0.08848052607644988,
            0.23451458444645476,
            0.42541136047056394,
            0.6334317797559347,
            0.8216091340006108,
            0.9528950441054435,
            1.0,
            0.9528950441054435,
            0.8216091340006108,
            0.6334317797559347,
            0.42541136047056394,
            0.23451458444645476,
            0.08848052607644988,
        ];
        check_window(WindowFunction::Kaiser { beta: 4.0 }, &expected);
    }

    #[test]
    fn test_kaiser_even() {
        // reference: scipy.signal.windows.kaiser(14, 4.0, sym=True)
        let expected = vec![
            0.08848052607644988,
            0.22142542587610076,
            0.39408799858012694,
            0.5857681621222717,
            0.7681541362093118,
            0.9111914713309941,
            0.9898205072696717,
            0.9898205072696717,
            0.9111914713309941,
            0.7681541362093118,
            0.5857681621222717,
            0.39408799858012694,
            0.22142542587610076,
            0.08848052607644988,
        ];
        check_window(WindowFunction::Kaiser { beta: 4.0 }, &expected);
    }

    #[test]
    fn test_bessel_i0_f64() {
        // reference: scipy.special.i0()
        assert_approx_f64(bessel_i0(0.0), 1.0);
        assert_approx_f64(bessel_i0(1.0), 1.2660658777520082);
        assert_approx_f64(bessel_i0(2.0), 2.279585302336067);
        assert_approx_f64(bessel_i0(3.0), 4.880792585865024);
        assert_approx_f64(bessel_i0(5.0), 27.239871823604442);
        assert_approx_f64(bessel_i0(10.0), 2815.716628466254);
        assert_approx_f64(bessel_i0(30.0), 781672297823.9775);
        assert_approx_f64(bessel_i0(-1.0), 1.2660658777520082);
        assert_approx_f64(bessel_i0(-2.0), 2.279585302336067);
        assert_approx_f64(bessel_i0(-3.0), 4.880792585865024);
        assert_approx_f64(bessel_i0(-5.0), 27.239871823604442);
        assert_approx_f64(bessel_i0(-10.0), 2815.716628466254);
        assert_approx_f64(bessel_i0(-30.0), 781672297823.9775);
    }

    fn assert_approx_f64(actual: f64, expected: f64) {
        assert!(
            (actual / expected - 1.0).abs() < 0.000001,
            "Expected {}, got {}",
            expected,
            actual
        );
    }

    #[test]
    fn test_bessel_i0_f32() {
        // reference: scipy.special.i0()
        assert_approx_f32(bessel_i0(0.0), 1.0);
        assert_approx_f32(bessel_i0(1.0), 1.2660658777520082);
        assert_approx_f32(bessel_i0(2.0), 2.279585302336067);
        assert_approx_f32(bessel_i0(3.0), 4.880792585865024);
        assert_approx_f32(bessel_i0(5.0), 27.239871823604442);
        assert_approx_f32(bessel_i0(10.0), 2815.716628466254);
        assert_approx_f32(bessel_i0(30.0), 781672297823.9775);
        assert_approx_f32(bessel_i0(-1.0), 1.2660658777520082);
        assert_approx_f32(bessel_i0(-2.0), 2.279585302336067);
        assert_approx_f32(bessel_i0(-3.0), 4.880792585865024);
        assert_approx_f32(bessel_i0(-5.0), 27.239871823604442);
        assert_approx_f32(bessel_i0(-10.0), 2815.716628466254);
        assert_approx_f32(bessel_i0(-30.0), 781672297823.9775);
    }

    fn assert_approx_f32(actual: f32, expected: f32) {
        assert!(
            (actual / expected - 1.0).abs() < 0.00001,
            "Expected {}, got {}",
            expected,
            actual
        );
    }

    fn check_window<T: Float + FloatConst + Debug>(wfunc: WindowFunction, sym_expected: &[T]) {
        let sym_len = sym_expected.len();
        let per_len = sym_len - 1;
        let iter_per = window::<T>(per_len, wfunc, Symmetry::Periodic);
        let iter_sym = window::<T>(sym_len, wfunc, Symmetry::Symmetric);
        for (idx, (actual, expected)) in iter_per.into_iter().zip(sym_expected).enumerate() {
            assert!(
                (actual - *expected).abs() < T::from(0.000001).unwrap(),
                "Diff at index {}, {:?} != {:?}",
                idx,
                actual,
                expected
            );
        }
        for (idx, (actual, expected)) in iter_sym.into_iter().zip(sym_expected).enumerate() {
            assert!(
                (actual - *expected).abs() < T::from(0.000001).unwrap(),
                "Diff at index {}, {:?} != {:?}",
                idx,
                actual,
                expected
            );
        }
    }
}
