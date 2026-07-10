import Link from 'next/link'

export default function Post() {
  return (
    <>
      <h1 id="post">Post</h1>
      <Link id="to-home" href="/">
        To home
      </Link>
    </>
  )
}
