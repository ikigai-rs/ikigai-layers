<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
  xmlns:r="http://www.w3.org/2005/sparql-results#" xmlns="http://www.w3.org/2005/Atom"
  exclude-result-prefixes="r">
  <!-- A feed layer's Atom 1.0 face (ledger #3284): the rows of the entries SELECT, in its SPARQL
       results XML. A stylesheet cannot be told the layer (no xsl:param reaches it), so the feed's
       own id and title come from the rows: the SELECT answers one even when no entry matches,
       naming the feed alone. Atom asks for a title and an updated date on every entry: an entry
       with no title says so, and updated is the time the host stamped when it was published. -->
  <xsl:output method="xml"/>
  <xsl:template match="/">
    <xsl:variable name="rows" select="/r:sparql/r:results/r:result"/>
    <xsl:variable name="entries" select="$rows[r:binding[@name='entry']]"/>
    <feed>
      <id><xsl:value-of select="$rows[1]/r:binding[@name='layer']/r:uri"/></id>
      <title><xsl:value-of select="$rows[1]/r:binding[@name='feedLabel']/r:literal"/></title>
      <updated>
        <xsl:choose>
          <xsl:when test="$entries">
            <xsl:value-of select="$entries[count($entries)]/r:binding[@name='issued']/r:literal"/>
          </xsl:when>
          <xsl:otherwise>1970-01-01T00:00:00Z</xsl:otherwise>
        </xsl:choose>
      </updated>
      <xsl:for-each select="$entries">
        <entry>
          <id><xsl:value-of select="r:binding[@name='entry']/r:uri"/></id>
          <title>
            <xsl:choose>
              <xsl:when test="r:binding[@name='title']">
                <xsl:value-of select="r:binding[@name='title']/r:literal"/>
              </xsl:when>
              <xsl:otherwise>(untitled)</xsl:otherwise>
            </xsl:choose>
          </title>
          <updated><xsl:value-of select="r:binding[@name='issued']/r:literal"/></updated>
          <published><xsl:value-of select="r:binding[@name='issued']/r:literal"/></published>
          <author><name><xsl:value-of select="r:binding[@name='creator']/r:uri"/></name></author>
          <!-- An entry without an alternate link must carry content, even when empty. -->
          <content type="text"><xsl:value-of select="r:binding[@name='content']/r:literal"/></content>
        </entry>
      </xsl:for-each>
    </feed>
  </xsl:template>
</xsl:stylesheet>
