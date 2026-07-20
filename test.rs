use std::error::Error;
use std::collections::HashMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {


    #[derive(Debug)]
    struct Product {
        id: i32,
        name: String,
        price: i32,
    }

    fn find_product_price_by_id(products: Vec<Product>, target_id: i32) -> Option<i32> {
        // 💡 이곳을 직접 채워보세요!
        // 힌트: products.into_iter().find(...).map(...)

        
        
    }

    
    let store_products = vec![
        Product { id: 101, name: String::from("노트북"), price: 1200 },
        Product { id: 102, name: String::from("마우스"), price: 50 },
        Product { id: 103, name: String::from("키보드"), price: 80 },
    ];

    // 1. 있는 상품 검색 테스트 (ID: 102 -> 마우스 가격 50)
    let mouse_price = find_product_price_by_id(store_products, 102);
    println!("마우스 가격 결과: {:?}", mouse_price);
    assert_eq!(mouse_price, Some(50));

    // 데이터 재선언
    let store_products = vec![
        Product { id: 101, name: String::from("노트북"), price: 1200 },
        Product { id: 102, name: String::from("마우스"), price: 50 },
        Product { id: 103, name: String::from("키보드"), price: 80 },
    ];

    // 2. 없는 상품 검색 테스트 (ID: 999 -> 존재하지 않음)
    let unknown_price = find_product_price_by_id(store_products, 999);
    println!("없는 상품 결과: {:?}", unknown_price);
    assert_eq!(unknown_price, None);

    println!("🎉 이번엔 진짜 직접 풀기 성공!");

    Ok(())
}

