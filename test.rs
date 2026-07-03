
#[tokio::main]
async fn main() -> anyhow::Result<()> {

    let missing_value: Option<i32> = Some(42);

    if let Some(val) = missing_value {
        println!("값이 들어있네요: {}", val);
    }

    let score: Option<i32> = None;

    let real_score = match score {
        Some(s) => s,
        None => 0,
    };

    let opt: Option<i32> = None;
    let val = opt.upwrap_or(0);

    struct Node {
        val: i32,
        left: Option<Box<Node>>,
        right: Option<Box<Node>>,
    }

    fn main() {
        let left_child = Node { val: 5, left: None, right: None };
        let right_child = Node { val: 15, left: None, right: None };

        let root = Node {
            val: 10,
            left: Some(Box::new(left_child)),
            right: Some(Box::new(right_child)),
        };

        if let Some(ref left_box) = root.left {
            println!("왼쪽 자식의 값: {}", left_box.val);
        }
    }

    struct Node {
        val: i32,
        next_idx: Option<usize>,
    }

    fn main() {
        let mut nodes: Vec<Node> = Vec::new();

        nodes.push(Node { val: 10, next_idx: Some(1) });
        nodes.push(Node { val: 20, next_idx: None });

        let first_node = &nodes[0];
        if let Some(next_room) = first_node.next_idx {
            let second_node = &nodes[next_room];
            println!("다음 노드의 값: {}", second_node.val);
        }
    }

}