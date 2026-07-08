use std::error::Error;
use std::collections::HashMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {


    fn search_max_prequence(vec: Vec<i32>) -> i32 {

        let mut count_map : HashMap<i32, i32> = HashMap::new();

        for v in vec {
            *count_map.entry(v).or_insert(0) += 1;
        }

        let mut count_key : i32 = 0;
        let mut count_value : i32 = 0;

        for (key, value) in count_map.iter() {
            if *value > count_value { 
                count_value = *value;
                count_key = *key;
            }
        }

        count_key

    }

    println!("a1 확인 결과 : {}", search_max_prequence(vec![1, 3, 3, 2, 3, 2]));
    Ok(())
}

