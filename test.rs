
fn main() -> Result<(), Box<dyn std::error::Error>> {


    // 장바구니에 담긴 아이템 구조체
    struct CartItem {
        name: String,
        category: String,
        price: i32,
    }

    // 할인 쿠폰 구조체
    struct Coupon {
        target_category: String,
        discount_amount: i32, // 할인해 줄 금액
    }

    fn calculate_total_price(cart: Vec<CartItem>, coupon: Option<Coupon>) -> i32 {
        // 💡 매운맛 힌트 요약:
        // 1. cart.into_iter()로 시작합니다.
        // 2. map 내부에서 각 item의 가격을 쿠폰 조건에 맞게 변경합니다.
        //    - coupon이 Some인지 None인지에 따라 분기가 필요합니다. (match 문이나 if let 활용 가능)
        //    - 쿠폰 카테고리와 일치하면 `price - discount_amount`를 하되, 0보다 작으면 0 처리합니다. (std::cmp::max 사용 가능)
        // 3. 마지막에 .sum::<i32>()로 마무리합니다.
        
        // 이 부분을 채워보세요!

        cart.into_iter().map(|i| match &coupon {
            Some(cp) => {
                if i.category == cp.target_category {
                    let discounted_price = i.price - cp.discount_amount;
                    if discounted_price < 0 {0} else {discounted_price}
                }else{
                    i.price
                }

            } 
            None => i.price
        }).sum::<i32>()
    }

    // 테스트용 장바구니 데이터 (총 정상가: 100 + 50 + 30 = 180)
    let my_cart = vec![
        CartItem { name: String::from("셔츠"), category: String::from("의류"), price: 100 },
        CartItem { name: String::from("청바지"), category: String::from("의류"), price: 50 },
        CartItem { name: String::from("키보드"), category: String::from("전자기기"), price: 30 },
    ];

    // 테스트 1: "의류" 카테고리 40원 할인 쿠폰이 있을 때
    // 셔츠(100-40=60) + 청바지(50-40=10) + 키보드(30, 할인제외) = 100원
    let clothing_coupon = Some(Coupon {
        target_category: String::from("의류"),
        discount_amount: 40,
    });
    
    let total_1 = calculate_total_price(my_cart, clothing_coupon);
    println!("쿠폰 적용 후 총 금액: {}원", total_1);
    assert_eq!(total_1, 100);


    // 데이터 재선언
    let my_cart = vec![
        CartItem { name: String::from("셔츠"), category: String::from("의류"), price: 100 },
        CartItem { name: String::from("청바지"), category: String::from("의류"), price: 50 },
        CartItem { name: String::from("키보드"), category: String::from("전자기기"), price: 30 },
    ];

    // 테스트 2: 쿠폰이 없을 때 (None) -> 정상가 그대로 합산
    let total_2 = calculate_total_price(my_cart, None);
    println!("쿠폰 미적용 총 금액: {}원", total_2);
    assert_eq!(total_2, 180);

    println!("🎉 매운맛 비즈니스 로직 테스트 통과 성공!");

    Ok(())
}



