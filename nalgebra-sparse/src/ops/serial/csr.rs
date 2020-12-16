use crate::csr::CsrMatrix;
use crate::ops::{Transpose};
use crate::SparseEntryMut;
use crate::ops::serial::{OperationError, OperationErrorType};
use nalgebra::{Scalar, DMatrixSlice, ClosedAdd, ClosedMul, DMatrixSliceMut};
use num_traits::{Zero, One};
use std::sync::Arc;
use std::borrow::Cow;

/// Sparse-dense matrix-matrix multiplication `C <- beta * C + alpha * trans(A) * trans(B)`.
pub fn spmm_csr_dense<'a, T>(c: impl Into<DMatrixSliceMut<'a, T>>,
                             beta: T,
                             alpha: T,
                             trans_a: Transpose,
                             a: &CsrMatrix<T>,
                             trans_b: Transpose,
                             b: impl Into<DMatrixSlice<'a, T>>)
    where
        T: Scalar + ClosedAdd + ClosedMul + Zero + One
{
    spmm_csr_dense_(c.into(), beta, alpha, trans_a, a, trans_b, b.into())
}

fn spmm_csr_dense_<T>(mut c: DMatrixSliceMut<T>,
                      beta: T,
                      alpha: T,
                      trans_a: Transpose,
                      a: &CsrMatrix<T>,
                      trans_b: Transpose,
                      b: DMatrixSlice<T>)
where
    T: Scalar + ClosedAdd + ClosedMul + Zero + One
{
    assert_compatible_spmm_dims!(c, a, b, trans_a, trans_b);

    if trans_a.to_bool() {
        // In this case, we have to pre-multiply C by beta
        c *= beta;

        for k in 0..a.nrows() {
            let a_row_k = a.row(k);
            for (&i, a_ki) in a_row_k.col_indices().iter().zip(a_row_k.values()) {
                let gamma_ki = alpha.inlined_clone() * a_ki.inlined_clone();
                let mut c_row_i = c.row_mut(i);
                if trans_b.to_bool() {
                    let b_col_k = b.column(k);
                    for (c_ij, b_jk) in c_row_i.iter_mut().zip(b_col_k.iter()) {
                        *c_ij += gamma_ki.inlined_clone() * b_jk.inlined_clone();
                    }
                } else {
                    let b_row_k = b.row(k);
                    for (c_ij, b_kj) in c_row_i.iter_mut().zip(b_row_k.iter()) {
                        *c_ij += gamma_ki.inlined_clone() * b_kj.inlined_clone();
                    }
                }
            }
        }
    } else {
        for j in 0..c.ncols() {
            let mut c_col_j = c.column_mut(j);
            for (c_ij, a_row_i) in c_col_j.iter_mut().zip(a.row_iter()) {
                let mut dot_ij = T::zero();
                for (&k, a_ik) in a_row_i.col_indices().iter().zip(a_row_i.values()) {
                    let b_contrib =
                        if trans_b.to_bool() { b.index((j, k)) } else { b.index((k, j)) };
                    dot_ij += a_ik.inlined_clone() * b_contrib.inlined_clone();
                }
                *c_ij = beta.inlined_clone() * c_ij.inlined_clone() + alpha.inlined_clone() * dot_ij;
            }
        }
    }
}

fn spadd_csr_unexpected_entry() -> OperationError {
    OperationError::from_type_and_message(
        OperationErrorType::InvalidPattern,
        String::from("Found entry in `a` that is not present in `c`."))
}

/// Sparse matrix addition `C <- beta * C + alpha * trans(A)`.
///
/// If the pattern of `c` does not accommodate all the non-zero entries in `a`, an error is
/// returned.
pub fn spadd_csr<T>(c: &mut CsrMatrix<T>,
                    beta: T,
                    alpha: T,
                    trans_a: Transpose,
                    a: &CsrMatrix<T>)
    -> Result<(), OperationError>
where
    T: Scalar + ClosedAdd + ClosedMul + Zero + One
{
    assert_compatible_spadd_dims!(c, a, trans_a);

    // TODO: Change CsrMatrix::pattern() to return `&Arc` instead of `Arc`
    if Arc::ptr_eq(&c.pattern(), &a.pattern()) {
        // Special fast path: The two matrices have *exactly* the same sparsity pattern,
        // so we only need to sum the value arrays
        for (c_ij, a_ij) in c.values_mut().iter_mut().zip(a.values()) {
            let (alpha, beta) = (alpha.inlined_clone(), beta.inlined_clone());
            *c_ij = beta * c_ij.inlined_clone() + alpha * a_ij.inlined_clone();
        }
        Ok(())
    } else {
        if trans_a.to_bool()
        {
            if beta != T::one() {
                for c_ij in c.values_mut() {
                    *c_ij *= beta.inlined_clone();
                }
            }

            for (i, a_row_i) in a.row_iter().enumerate() {
                for (&j, a_val) in a_row_i.col_indices().iter().zip(a_row_i.values()) {
                    let a_val = a_val.inlined_clone();
                    let alpha = alpha.inlined_clone();
                    match c.index_entry_mut(j, i) {
                        SparseEntryMut::NonZero(c_ji) => { *c_ji += alpha * a_val }
                        SparseEntryMut::Zero => return Err(spadd_csr_unexpected_entry()),
                    }
                }
            }
        } else {
            for (mut c_row_i, a_row_i) in c.row_iter_mut().zip(a.row_iter()) {
                if beta != T::one() {
                    for c_ij in c_row_i.values_mut() {
                        *c_ij *= beta.inlined_clone();
                    }
                }

                let (mut c_cols, mut c_vals) = c_row_i.cols_and_values_mut();
                let (a_cols, a_vals) = (a_row_i.col_indices(), a_row_i.values());

                for (a_col, a_val) in a_cols.iter().zip(a_vals) {
                    // TODO: Use exponential search instead of linear search.
                    // If C has substantially more entries in the row than A, then a line search
                    // will needlessly visit many entries in C.
                    let (c_idx, _) = c_cols.iter()
                        .enumerate()
                        .find(|(_, c_col)| *c_col == a_col)
                        .ok_or_else(spadd_csr_unexpected_entry)?;
                    c_vals[c_idx] += alpha.inlined_clone() * a_val.inlined_clone();
                    c_cols = &c_cols[c_idx ..];
                    c_vals = &mut c_vals[c_idx ..];
                }
            }
        }
        Ok(())
    }
}

fn spmm_csr_unexpected_entry() -> OperationError {
    OperationError::from_type_and_message(
        OperationErrorType::InvalidPattern,
        String::from("Found unexpected entry that is not present in `c`."))
}

/// Sparse-sparse matrix multiplication, `C <- beta * C + alpha * op(A) * op(B)`.
pub fn spmm_csr<'a, T>(
    c: &mut CsrMatrix<T>,
    beta: T,
    alpha: T,
    trans_a: Transpose,
    a: &CsrMatrix<T>,
    trans_b: Transpose,
    b: &CsrMatrix<T>)
-> Result<(), OperationError>
where
    T: Scalar + ClosedAdd + ClosedMul + Zero + One
{
    assert_compatible_spmm_dims!(c, a, b, trans_a, trans_b);

    if !trans_a.to_bool() && !trans_b.to_bool() {
        for (mut c_row_i, a_row_i) in c.row_iter_mut().zip(a.row_iter()) {
            for c_ij in c_row_i.values_mut() {
                *c_ij = beta.inlined_clone() * c_ij.inlined_clone();
            }

            for (&k, a_ik) in a_row_i.col_indices().iter().zip(a_row_i.values()) {
                let b_row_k = b.row(k);
                let (mut c_row_i_cols, mut c_row_i_values) = c_row_i.cols_and_values_mut();
                let alpha_aik = alpha.inlined_clone() * a_ik.inlined_clone();
                for (j, b_kj) in b_row_k.col_indices().iter().zip(b_row_k.values()) {
                    // Determine the location in C to append the value
                    let (c_local_idx, _) = c_row_i_cols.iter()
                        .enumerate()
                        .find(|(_, c_col)| *c_col == j)
                        .ok_or_else(spmm_csr_unexpected_entry)?;

                    c_row_i_values[c_local_idx] += alpha_aik.inlined_clone() * b_kj.inlined_clone();
                    c_row_i_cols = &c_row_i_cols[c_local_idx ..];
                    c_row_i_values = &mut c_row_i_values[c_local_idx ..];
                }
            }
        }
        Ok(())
    } else {
        // Currently we handle transposition by explicitly precomputing transposed matrices
        // and calling the operation again without transposition
        // TODO: At least use workspaces to allow control of allocations. Maybe
        // consider implementing certain patterns (like A^T * B) explicitly
        let (a, b) = {
            use Cow::*;
            match (trans_a, trans_b) {
                (Transpose(false), Transpose(false)) => unreachable!(),
                (Transpose(true), Transpose(false)) => (Owned(a.transpose()), Borrowed(b)),
                (Transpose(false), Transpose(true)) => (Borrowed(a), Owned(b.transpose())),
                (Transpose(true), Transpose(true)) => (Owned(a.transpose()), Owned(b.transpose()))
            }
        };

        spmm_csr(c, beta, alpha, Transpose(false), a.as_ref(), Transpose(false), b.as_ref())
    }
}

