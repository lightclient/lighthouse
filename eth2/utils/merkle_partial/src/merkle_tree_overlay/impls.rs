use super::MerkleTreeOverlay;
use crate::field::{Basic, Composite, Leaf, Node};
use crate::tree_arithmetic::zeroed::{general_index_to_subtree, relative_depth, root_from_depth};
use crate::tree_arithmetic::{log_base_two, next_power_of_two};
use crate::{NodeIndex, BYTES_PER_CHUNK};
use ethereum_types::U256;
use ssz_types::VariableList;
use typenum::Unsigned;

macro_rules! impl_merkle_overlay_for_basic_type {
    ($type: ident, $bit_size: expr) => {
        impl MerkleTreeOverlay for $type {
            fn height() -> u8 {
                0
            }

            fn first_leaf() -> NodeIndex {
                0
            }

            fn last_leaf() -> NodeIndex {
                0
            }

            fn get_node(index: NodeIndex) -> Node {
                match index {
                    0 => Node::Leaf(Leaf::Basic(vec![Basic {
                        ident: "".to_owned(),
                        index,
                        size: ($bit_size / 32) as u8,
                        offset: 0,
                    }])),
                    _ => Node::Unattached(index),
                }
            }
        }
    };
}

impl_merkle_overlay_for_basic_type!(bool, 8);
impl_merkle_overlay_for_basic_type!(u8, 8);
impl_merkle_overlay_for_basic_type!(u16, 16);
impl_merkle_overlay_for_basic_type!(u32, 32);
impl_merkle_overlay_for_basic_type!(u64, 64);
impl_merkle_overlay_for_basic_type!(u128, 128);
impl_merkle_overlay_for_basic_type!(U256, 256);
impl_merkle_overlay_for_basic_type!(usize, std::mem::size_of::<usize>());

/// Implements the `MerkleTreeOverlay` trait for `ssz_types::VariableList`.
///
/// The full specification of the merkle tree structure can be found in the SSZ documentation:
/// https://github.com/ethereum/eth2.0-specs/blob/dev/specs/simple-serialize.md#merkleization
///
/// Below is a visual representation of the structure:
///
///             root(0)
///           /         \
///      data_root(1)  len(2)
///        /   \
///      . . . . .  <= intermediate nodes
///      / \   / \
///     x   x x   x <= leaf nodes
///

impl<T: MerkleTreeOverlay, N: Unsigned> MerkleTreeOverlay for VariableList<T, N> {
    fn height() -> u8 {
        let num_leaves = next_power_of_two(N::to_u64());
        let data_tree_height = log_base_two(num_leaves);

        // Add one to account for the data root and the length of the list.
        (data_tree_height as u8) + 1
    }

    fn first_leaf() -> NodeIndex {
        (1_u64 << Self::height()) - 1
    }

    fn last_leaf() -> NodeIndex {
        // last_leaf = 2^h + (2^h / 2) - 2
        //
        // The last leaf in the data tree would be first leaf, plus half the total number of leaves,
        // because the data tree is only in the left side of the full tree. The leaves afterwards
        // are children of the length node and are unattached.
        (1_u64 << Self::height()) + (1_u64 << (Self::height() - 1)) - 2
    }

    fn get_node(index: NodeIndex) -> Node {
        let first_leaf = Self::first_leaf();
        let first_internal = 3;
        let last_internal = (1_u64 << Self::height()) - 2;
        let last_leaf = Self::last_leaf();

        // The `index` can either i) exist within the current object and directly match a local
        // index in `0..=Self::last_leaf()` or ii) is a child of one of the leaves in the current
        // tree. If `index` is a child, call `get_node` on the child object `T` with an index
        // translated to the child object's index space.
        if index == 0 {
            Node::Composite(Composite {
                ident: "".to_owned(),
                index: 0,
                height: Self::height().into(),
            })
        } else if index == 1 {
            Node::Intermediate(index)
        } else if index == 2 {
            Node::Leaf(Leaf::Length(Basic {
                ident: "len".to_string(),
                index: index,
                size: 32,
                offset: 0,
            }))
        } else if (first_internal..=last_internal).contains(&index) {
            Node::Intermediate(index)
        } else if (first_leaf..=last_leaf).contains(&index) {
            let node_type = T::get_node(0);

            match node_type {
                Node::Leaf(Leaf::Basic(_)) => {
                    let item_size = std::mem::size_of::<T>() as u8;
                    let items_per_chunk = BYTES_PER_CHUNK as u8 / item_size;

                    Node::Leaf(Leaf::Basic(
                        vec![Basic::default(); items_per_chunk as usize]
                            .iter()
                            .enumerate()
                            .map(|(i, _)| Basic {
                                ident: ((index - Self::first_leaf()) * items_per_chunk as u64
                                    + i as u64)
                                    .to_string(),
                                index: index,
                                size: item_size,
                                offset: i as u8 * item_size,
                            })
                            .collect(),
                    ))
                }
                Node::Composite(c) => Node::Composite(Composite {
                    ident: (index - Self::first_leaf()).to_string(),
                    index,
                    height: c.height,
                }),
                _ => unreachable!("Leaf should either be composite or basic value"),
            }
        } else {
            // If no match at this point, the node must be in one of `T`'s subtrees or it's not
            // attached to the current tree anywhere.

            let subtree_root = root_from_depth(index, relative_depth(first_leaf, index));
            let subtree_index = general_index_to_subtree(subtree_root, index);

            if (first_leaf..=last_leaf).contains(&subtree_root) {
                // Call `T::get_node` & replace the node's index with the current known index.
                replace_index(T::get_node(subtree_index), index)
            } else {
                Node::Unattached(index)
            }
        }
    }
}

/// Returns a copy of `node` with all its index values changed to `index`.
fn replace_index(node: Node, index: NodeIndex) -> Node {
    match node {
        Node::Composite(c) => Node::Composite(Composite {
            ident: c.ident,
            index: index,
            height: c.height,
        }),
        Node::Leaf(Leaf::Basic(b)) => Node::Leaf(Leaf::Basic(
            b.iter()
                .cloned()
                .map(|mut x| {
                    x.index = index;
                    x
                })
                .collect(),
        )),
        Node::Leaf(Leaf::Length(b)) => Node::Leaf(Leaf::Length(Basic {
            ident: b.ident,
            index: index,
            size: 32,
            offset: 0,
        })),
        Node::Leaf(Leaf::Padding()) => Node::Leaf(Leaf::Padding()),
        Node::Unattached(_) => Node::Unattached(index),
        Node::Intermediate(_) => Node::Intermediate(index),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use typenum::{U2, U4, U8};

    #[test]
    fn variable_list_overlay() {
        type T = VariableList<U256, U8>;
        assert_eq!(
            T::get_node(0),
            Node::Composite(Composite {
                ident: "".to_owned(),
                index: 0,
                height: T::height(),
            })
        );

        assert_eq!(T::get_node(1), Node::Intermediate(1));
        assert_eq!(T::get_node(10), Node::Intermediate(10));

        assert_eq!(
            T::get_node(2),
            Node::Leaf(Leaf::Length(Basic {
                ident: "len".to_string(),
                index: 2,
                size: 32,
                offset: 0
            }))
        );

        assert_eq!(
            T::get_node(15),
            Node::Leaf(Leaf::Basic(vec![Basic {
                ident: 0.to_string(),
                index: 15,
                size: 32,
                offset: 0
            }]))
        );

        assert_eq!(
            T::get_node(18),
            Node::Leaf(Leaf::Basic(vec![Basic {
                ident: 3.to_string(),
                index: 18,
                size: 32,
                offset: 0
            }]))
        );

        assert_eq!(
            T::get_node(22),
            Node::Leaf(Leaf::Basic(vec![Basic {
                ident: 7.to_string(),
                index: 22,
                size: 32,
                offset: 0
            }]))
        );

        assert_eq!(T::get_node(23), Node::Unattached(23));
        assert_eq!(T::get_node(31), Node::Unattached(31));
        assert_eq!(T::get_node(2_u64.pow(32)), Node::Unattached(2_u64.pow(32)));
    }

    #[test]
    fn nested_variable_list_overlay() {
        type T = VariableList<VariableList<VariableList<U256, U2>, U2>, U4>;

        assert_eq!(
            T::get_node(0),
            Node::Composite(Composite {
                ident: "".to_owned(),
                index: 0,
                height: T::height(),
            })
        );

        assert_eq!(T::get_node(1), Node::Intermediate(1));
        assert_eq!(T::get_node(4), Node::Intermediate(4));

        assert_eq!(
            T::get_node(2),
            Node::Leaf(Leaf::Length(Basic {
                ident: "len".to_string(),
                index: 2,
                size: 32,
                offset: 0
            }))
        );

        assert_eq!(
            T::get_node(16),
            Node::Leaf(Leaf::Length(Basic {
                ident: "len".to_string(),
                index: 16,
                size: 32,
                offset: 0
            }))
        );

        assert_eq!(
            T::get_node(22),
            Node::Leaf(Leaf::Length(Basic {
                ident: "len".to_string(),
                index: 22,
                size: 32,
                offset: 0
            }))
        );

        assert_eq!(
            T::get_node(32),
            Node::Composite(Composite {
                ident: 1.to_string(),
                index: 32,
                height: 2
            })
        );

        assert_eq!(
            T::get_node(176),
            Node::Leaf(Leaf::Basic(vec![Basic {
                ident: 1.to_string(),
                index: 176,
                size: 32,
                offset: 0
            }]))
        );

        assert_eq!(T::get_node(45), Node::Unattached(45));
        assert_eq!(T::get_node(177), Node::Unattached(177));
        assert_eq!(T::get_node(123456789), Node::Unattached(123456789));
    }
}
