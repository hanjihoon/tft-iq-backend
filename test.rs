

fn main() -> Result<(), Box<dyn std::error::Error>> {


    let numbers = vec![ 1, 2, 3, 4, 5, 6];

    let number_ten_times: Vec<i32> = numbers.into_iter()
    .filter(|n| n % 2 == 0)
    .map(|n| n * 10)
    .collect();

    println!("{:?}", number_ten_times);


    Ok(())
}

