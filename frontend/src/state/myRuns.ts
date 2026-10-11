// Runs I'm ferrying. The contract only lists open runs, so after I close a
// window the run is remembered here until it's delivered (CONTRACT_REQUESTS CR-3).
const KEY = 'ferry.myRuns'

export function rememberRun(id: string) {
  const list = myRuns()
  if (!list.includes(id)) save([id, ...list].slice(0, 10))
}

export function forgetRun(id: string) {
  save(myRuns().filter((x) => x !== id))
}

export function myRuns(): string[] {
  try {
    return JSON.parse(localStorage.getItem(KEY) ?? '[]')
  } catch {
    return []
  }
}

function save(list: string[]) {
  try {
    localStorage.setItem(KEY, JSON.stringify(list))
  } catch {
    /* memory only */
  }
}
