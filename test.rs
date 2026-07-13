use std::error::Error;
use std::collections::HashMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {

    fn square_not_divisible_by_three(vec: Vec<i32>) -> Vec<i32> {
        
        vec.into_iter().filter(|n| n % 3 != 0)
        .map(|n| n * n)
        .collect()

    }

    fn uppercase_long_words(vec: Vec<String>) -> Vec<String> {
        
        vec.into_iter().filter(|str| str.len() >= 5)
        .map(|str| str.to_uppercase())
        .collect()

    }

    let input = vec![1, 2, 3, 4, 5, 6];
    let output = square_not_divisible_by_three(input);
    println!("{:?}", output); // 예상 결과: [4, 8] (2와 4가 각각 2배가 됨)

    Ok(())
}

