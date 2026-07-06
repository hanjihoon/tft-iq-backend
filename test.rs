

fn main() -> Result<(), Box<dyn std::error::Error>> {


    let matrix = vec![vec![1, 2], vec![3, 4], vec![5, 6]];

    let flat_list: Vec<i32> = matrix.into_iter().flatten().collect();

    println!("{:?}", flat_list);


    Ok(())
}

