# WindowFunctions

Window functions for apodization.

## Overview
The `WindowFunctions` library provides a variety of window functions used in signal processing to reduce spectral leakage. These window functions are essential for applications like spectrum analysis and filter design, where improving the frequency characteristics of the signal is crucial.

## Example, print all the values from the window iterator
```rust
extern crate windowfunctions;
use windowfunctions::{WindowFunction, Symmetry, window};

fn main() {
    let length = 1024;
    let window_type = WindowFunction::Hamming;
    let symmetry = Symmetry::Symmetric;

    let window_iter = window::<f32>(length, window_type, symmetry);

    for value in window_iter {
        println!("{}", value);
    }
}
```

## Example, store the window in a vector
```rust
extern crate windowfunctions;
use windowfunctions::{WindowFunction, Symmetry, window};

fn main() {
    let length = 1024;
    let window_type = WindowFunction::Hamming;
    let symmetry = Symmetry::Symmetric;

    let window_iter = window::<f32>(length, window_type, symmetry);

    let window_vec: Vec<f32> = window_iter.into_iter().collect();
}
```

## License
This library is licensed under the MIT License. See the LICENSE file for more details.

## Contributions
Contributions are welcome! Feel free to open issues or submit pull requests on the GitHub repository.
