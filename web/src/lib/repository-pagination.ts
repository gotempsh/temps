export function hasNextRepositoryPage(
  totalCount: number,
  currentPage: number,
  itemsPerPage: number
): boolean {
  if (totalCount <= 0 || currentPage <= 0 || itemsPerPage <= 0) return false
  return currentPage * itemsPerPage < totalCount
}
